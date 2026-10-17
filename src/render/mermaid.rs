//! Mermaid flowcharts in Markdown (```` ```mermaid ```` blocks): a parser for
//! the flowchart syntax and a small layered layout that turns it into boxes,
//! lines and labels in pixels. Drawing them is `ui.rs`'s job.
//!
//! What is read: `flowchart`/`graph` with a direction (TD, TB, BT, LR, RL);
//! nodes with the usual shapes (`A[box]`, `A(round)`, `A([stadium])`,
//! `A[[subroutine]]`, `A[(database)]`, `A((circle))`, `A{diamond}`,
//! `A{{hexagon}}`, `A>flag]`, `A[/lean/]`, `A[\lean\]`, `A[/trapezoid\]`,
//! `A[\trapezoid/]`), quoted labels and `<br>`; links (`-->`, `---`, `-.->`,
//! `==>`, `~~~`, `<-->`, `--o`, `--x`) with text (`-->|text|`, `-- text -->`),
//! chains (`A --> B --> C`) and `&` (`A & B --> C`); `subgraph ... end`.
//! Styling statements (`classDef`, `class`, `style`, `linkStyle`, `click`)
//! are skipped. Other diagram types are refused with a message.
//!
//! The layout: nodes are put in layers by the longest path from the roots
//! (links that close a cycle are turned around for that), links that skip
//! layers get invisible waypoints, each layer is ordered by the average
//! position of its neighbours, and links are drawn as right-angled lines
//! through the gaps between layers.

use std::collections::HashMap;

use regex::Regex;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Direction {
    Down,
    Up,
    Right,
    Left,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Shape {
    Rect,
    Round,
    Stadium,
    Subroutine,
    Cylinder,
    Circle,
    Diamond,
    Hexagon,
    Flag,
    Lean,
    LeanLeft,
    Trapezoid,
    TrapezoidDown,
}

/// How a link is drawn.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Stroke {
    Solid,
    Dotted,
    Thick,
    Invisible,
}

/// The end of a link.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Head {
    None,
    Arrow,
    Circle,
    Cross,
}

#[derive(Debug, Clone)]
pub struct Node {
    /// The name links use; read by the tests only, the label is what shows.
    #[allow(dead_code)]
    pub id: String,
    pub label: String,
    pub shape: Shape,
}

#[derive(Debug, Clone)]
pub struct Edge {
    pub from: usize,
    pub to: usize,
    pub label: Option<String>,
    pub stroke: Stroke,
    pub start: Head,
    pub end: Head,
}

#[derive(Debug, Clone)]
pub struct Group {
    pub title: String,
    /// Nodes first mentioned inside this subgraph (not in a nested one).
    pub members: Vec<usize>,
    pub parent: Option<usize>,
}

#[derive(Debug, Clone)]
pub struct Graph {
    pub direction: Direction,
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
    pub groups: Vec<Group>,
}

// ---------------------------------------------------------------------------
// Parsing
// ---------------------------------------------------------------------------

/// Node shapes by their brackets. Longer openings come first, and shapes
/// sharing an opening are told apart by their closing.
const SHAPES: &[(&str, &str, Shape)] = &[
    ("(((", ")))", Shape::Circle),
    ("([", "])", Shape::Stadium),
    ("[[", "]]", Shape::Subroutine),
    ("[(", ")]", Shape::Cylinder),
    ("((", "))", Shape::Circle),
    ("{{", "}}", Shape::Hexagon),
    ("[/", "/]", Shape::Lean),
    ("[/", "\\]", Shape::Trapezoid),
    ("[\\", "\\]", Shape::LeanLeft),
    ("[\\", "/]", Shape::TrapezoidDown),
    ("[", "]", Shape::Rect),
    ("(", ")", Shape::Round),
    ("{", "}", Shape::Diamond),
    (">", "]", Shape::Flag),
];

pub fn parse(source: &str) -> Result<Graph, String> {
    let mut parser = Parser {
        graph: Graph { direction: Direction::Down, nodes: Vec::new(), edges: Vec::new(), groups: Vec::new() },
        ids: HashMap::new(),
        open_groups: Vec::new(),
        link_with_text: Regex::new(r"^(<|o|x)?(--|==|-\.)([^-=.>|][^>|]*?)(-{2,}|={2,}|\.+-)(>|o|x)?").expect("valid regex"),
        link: Regex::new(r"^(<|o|x)?(-{2,}|={2,}|-?\.+-|~{3,})(>|o|x)?(?:\s*\|([^|]*)\|)?").expect("valid regex"),
    };
    let mut header = false;
    for (number, raw) in source.lines().enumerate() {
        let line = raw.split("%%").next().unwrap_or("").trim();
        for statement in split_statements(line) {
            let statement = statement.trim();
            if statement.is_empty() {
                continue;
            }
            if !header {
                let mut words = statement.split_whitespace();
                if !matches!(words.next(), Some("flowchart" | "graph")) {
                    return Err("only flowcharts are drawn (`flowchart TD` or `graph LR`)".to_string());
                }
                parser.graph.direction = match words.next() {
                    Some("BT") => Direction::Up,
                    Some("LR") => Direction::Right,
                    Some("RL") => Direction::Left,
                    _ => Direction::Down,
                };
                header = true;
                continue;
            }
            parser.statement(statement).map_err(|e| format!("mermaid, line {}: {e}", number + 1))?;
        }
    }
    if !header {
        return Err("empty diagram".to_string());
    }
    if !parser.open_groups.is_empty() {
        return Err("a `subgraph` has no `end`".to_string());
    }
    Ok(parser.graph)
}

/// Split a line on `;`, except inside quotes.
fn split_statements(line: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let (mut start, mut quoted) = (0, false);
    for (i, c) in line.char_indices() {
        match c {
            '"' => quoted = !quoted,
            ';' if !quoted => {
                out.push(&line[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    out.push(&line[start..]);
    out
}

struct Parser {
    graph: Graph,
    ids: HashMap<String, usize>,
    open_groups: Vec<usize>,
    link_with_text: Regex,
    link: Regex,
}

impl Parser {
    fn statement(&mut self, text: &str) -> Result<(), String> {
        let first = text.split_whitespace().next().unwrap_or("");
        match first {
            "subgraph" => {
                let title = subgraph_title(text["subgraph".len()..].trim());
                let parent = self.open_groups.last().copied();
                self.graph.groups.push(Group { title, members: Vec::new(), parent });
                self.open_groups.push(self.graph.groups.len() - 1);
                return Ok(());
            }
            "end" => {
                self.open_groups.pop().ok_or("`end` without a `subgraph`")?;
                return Ok(());
            }
            "direction" | "classDef" | "class" | "style" | "linkStyle" | "click" | "accTitle" | "accDescr" => return Ok(()),
            _ => {}
        }
        // A chain: nodes (joined by `&`) separated by links.
        let mut text = text;
        let mut previous = self.node_group(&mut text)?;
        loop {
            text = text.trim_start();
            if text.is_empty() {
                return Ok(());
            }
            let (stroke, start, end, label, used) =
                self.read_link(text).ok_or_else(|| format!("expected a link like `-->` at `{text}`"))?;
            text = &text[used..];
            let next = self.node_group(&mut text)?;
            for &from in &previous {
                for &to in &next {
                    self.graph.edges.push(Edge { from, to, label: label.clone(), stroke, start, end });
                }
            }
            previous = next;
        }
    }

    /// `A`, `A[label]`, `A & B{label}`: the nodes, created or updated.
    fn node_group(&mut self, text: &mut &str) -> Result<Vec<usize>, String> {
        let mut nodes = Vec::new();
        loop {
            *text = text.trim_start();
            let id_len = id_length(text);
            if id_len == 0 {
                return Err(format!("expected a node at `{text}`"));
            }
            let id = text[..id_len].to_string();
            *text = &text[id_len..];
            let shape = read_shape(text)?;
            // `:::className` after a node only styles it.
            if let Some(rest) = text.strip_prefix(":::") {
                *text = &rest[id_length(rest)..];
            }
            nodes.push(self.node(id, shape));
            *text = text.trim_start();
            match text.strip_prefix('&') {
                Some(rest) => *text = rest,
                None => return Ok(nodes),
            }
        }
    }

    fn node(&mut self, id: String, shape: Option<(Shape, String)>) -> usize {
        let index = match self.ids.get(&id) {
            Some(&index) => index,
            None => {
                self.graph.nodes.push(Node { id: id.clone(), label: id.clone(), shape: Shape::Rect });
                let index = self.graph.nodes.len() - 1;
                self.ids.insert(id, index);
                if let Some(&group) = self.open_groups.last() {
                    self.graph.groups[group].members.push(index);
                }
                index
            }
        };
        if let Some((shape, label)) = shape {
            self.graph.nodes[index].shape = shape;
            self.graph.nodes[index].label = label;
        }
        index
    }

    /// A link at the start of `text`: its stroke, heads, label and length in bytes.
    fn read_link(&self, text: &str) -> Option<(Stroke, Head, Head, Option<String>, usize)> {
        let (start, body, end, label, mut used) = if let Some(found) = self.link_with_text.captures(text) {
            let body = format!("{}{}", &found[2], &found[4]);
            let label = found[3].trim().to_string();
            (found.get(1).map(|m| m.as_str()), body, found.get(5).map(|m| m.as_str()), Some(label), found[0].len())
        } else {
            let found = self.link.captures(text)?;
            let label = found.get(4).map(|m| m.as_str().trim().to_string());
            (found.get(1).map(|m| m.as_str()), found[2].to_string(), found.get(3).map(|m| m.as_str()), label, found[0].len())
        };
        let mut end = end;
        // `--o` and `--x` are heads only when a space or the end follows:
        // in `--- xray`, the x starts a node.
        if let Some(head @ ("o" | "x")) = end {
            let after = text[used..].chars().next();
            if after.is_some_and(|c| c.is_alphanumeric() || c == '_') && !text[..used].ends_with('|') {
                used -= head.len();
                end = None;
            }
        }
        let stroke = if body.contains('~') {
            Stroke::Invisible
        } else if body.contains('=') {
            Stroke::Thick
        } else if body.contains('.') {
            Stroke::Dotted
        } else {
            Stroke::Solid
        };
        let head = |mark: Option<&str>| match mark {
            Some(">" | "<") => Head::Arrow,
            Some("o") => Head::Circle,
            Some("x") => Head::Cross,
            _ => Head::None,
        };
        let label = label.filter(|l| !l.is_empty()).map(|l| clean_label(&l));
        Some((stroke, head(start), head(end), label, used))
    }
}

/// Length in bytes of the node id at the start of `text`. A `-` belongs to
/// the id only when a letter or digit follows (`node-1`, not `A-->B`).
fn id_length(text: &str) -> usize {
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i].1;
        let next_is_word = chars.get(i + 1).is_some_and(|(_, n)| n.is_alphanumeric());
        if c.is_alphanumeric() || c == '_' || (c == '-' && i > 0 && next_is_word) {
            i += 1;
        } else {
            break;
        }
    }
    chars.get(i).map(|(byte, _)| *byte).unwrap_or(text.len())
}

/// The shape and label right after a node id, if any (`[label]`, `{label}`...).
fn read_shape(text: &mut &str) -> Result<Option<(Shape, String)>, String> {
    for &(open, close, shape) in SHAPES {
        let Some(rest) = text.strip_prefix(open) else { continue };
        let inner = rest.trim_start();
        let (label, after) = if let Some(quoted) = inner.strip_prefix('"') {
            let Some(end) = quoted.find('"') else { return Err("a label has no closing quote".to_string()) };
            let after = quoted[end + 1..].trim_start();
            match after.strip_prefix(close) {
                Some(after) => (&quoted[..end], after),
                None => continue,
            }
        } else {
            match rest.find(close) {
                Some(end) => (&rest[..end], &rest[end + close.len()..]),
                None => continue,
            }
        };
        *text = after;
        return Ok(Some((shape, clean_label(label))));
    }
    Ok(None)
}

/// `subgraph id [Title]`, `subgraph "Title"` or `subgraph id`.
fn subgraph_title(text: &str) -> String {
    let title = match (text.find('['), text.rfind(']')) {
        (Some(open), Some(close)) if open < close => &text[open + 1..close],
        _ => text,
    };
    clean_label(title)
}

/// Quotes off, `<br>` to line breaks, the common entities decoded.
fn clean_label(text: &str) -> String {
    let text = text.trim();
    let text = text.strip_prefix('"').and_then(|t| t.strip_suffix('"')).unwrap_or(text);
    let text = text.strip_prefix('`').and_then(|t| t.strip_suffix('`')).unwrap_or(text);
    let mut out = text.to_string();
    for br in ["<br/>", "<br />", "<br>", "<BR>", "\\n"] {
        out = out.replace(br, "\n");
    }
    out.replace("#quot;", "\"").replace("&quot;", "\"").replace("&amp;", "&").replace("&lt;", "<").replace("&gt;", ">")
}

// ---------------------------------------------------------------------------
// Layout
// ---------------------------------------------------------------------------

/// A laid-out diagram, in pixels from its top-left corner.
#[derive(Debug, Clone)]
pub struct Diagram {
    pub width: f32,
    pub height: f32,
    pub groups: Vec<GroupBox>,
    pub edges: Vec<EdgeLine>,
    pub nodes: Vec<NodeBox>,
}

#[derive(Debug, Clone)]
pub struct NodeBox {
    pub shape: Shape,
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub lines: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct EdgeLine {
    /// Right-angled path from the start of the link to its end.
    pub points: Vec<(f32, f32)>,
    pub stroke: Stroke,
    pub start: Head,
    pub end: Head,
    /// Label lines and the center they are drawn around.
    pub label: Option<(Vec<String>, f32, f32)>,
}

#[derive(Debug, Clone)]
pub struct GroupBox {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub title: String,
}

/// Parse and lay out a flowchart for a font of `cell_w` by `cell_h` pixels.
pub fn render(source: &str, cell_w: f32, cell_h: f32) -> Result<Diagram, String> {
    let graph = parse(source)?;
    if graph.nodes.is_empty() {
        return Err("the flowchart has no nodes".to_string());
    }
    Ok(layout(&graph, cell_w, cell_h))
}

/// Width and height of a node's box, from its label and shape.
fn node_size(node: &Node, cell_w: f32, cell_h: f32) -> (f32, f32) {
    let lines: Vec<&str> = node.label.lines().collect();
    let chars = lines.iter().map(|l| l.chars().count()).max().unwrap_or(0).max(1) as f32;
    let text_w = chars * cell_w;
    let text_h = lines.len().max(1) as f32 * cell_h;
    let (pad_w, pad_h) = (cell_w * 2.0, cell_h * 0.8);
    let (w, h) = (text_w + pad_w * 2.0, text_h + pad_h * 2.0);
    match node.shape {
        // Text fits in a diamond twice its size (half along each diagonal).
        Shape::Diamond => (text_w * 2.0 + pad_w, text_h * 2.0 + pad_h),
        Shape::Circle => {
            let d = (text_w * text_w + text_h * text_h).sqrt() + pad_w;
            (d, d)
        }
        Shape::Stadium | Shape::Hexagon | Shape::Lean | Shape::LeanLeft | Shape::Trapezoid | Shape::TrapezoidDown => (w + h, h),
        Shape::Flag => (w + h / 2.0, h),
        Shape::Cylinder => (w, h + cell_h),
        Shape::Subroutine => (w + cell_w * 2.0, h),
        Shape::Rect | Shape::Round => (w, h),
    }
}

/// One slot in a layer: a node, or a waypoint of a link that skips layers.
#[derive(Debug, Clone, Copy)]
enum Item {
    Node(usize),
    Waypoint,
}

pub fn layout(graph: &Graph, cell_w: f32, cell_h: f32) -> Diagram {
    let vertical = matches!(graph.direction, Direction::Down | Direction::Up);
    let count = graph.nodes.len();
    let sizes: Vec<(f32, f32)> = graph.nodes.iter().map(|n| node_size(n, cell_w, cell_h)).collect();
    // Sizes along the layer axis (`r`, down the page for TD) and across it (`o`).
    let along_r = |(w, h): (f32, f32)| if vertical { h } else { w };
    let along_o = |(w, h): (f32, f32)| if vertical { w } else { h };

    // 1. Layers. Links that close a cycle are followed backwards.
    let mut outgoing: Vec<Vec<usize>> = vec![Vec::new(); count];
    for (i, edge) in graph.edges.iter().enumerate() {
        outgoing[edge.from].push(i);
    }
    let mut reversed = vec![false; graph.edges.len()];
    let mut state = vec![0u8; count]; // 0 new, 1 on the path, 2 done
    for root in 0..count {
        if state[root] != 0 {
            continue;
        }
        let mut stack = vec![(root, 0usize)];
        state[root] = 1;
        while let Some((node, next)) = stack.pop() {
            if let Some(&edge) = outgoing[node].get(next) {
                stack.push((node, next + 1));
                let to = graph.edges[edge].to;
                match state[to] {
                    0 => {
                        state[to] = 1;
                        stack.push((to, 0));
                    }
                    1 => reversed[edge] = true,
                    _ => {}
                }
            } else {
                state[node] = 2;
            }
        }
    }
    // Links as followed for the layers (self-links stay out).
    let forward: Vec<Option<(usize, usize)>> = graph
        .edges
        .iter()
        .zip(&reversed)
        .map(|(e, &rev)| (e.from != e.to).then_some(if rev { (e.to, e.from) } else { (e.from, e.to) }))
        .collect();
    let mut rank = vec![0usize; count];
    let mut incoming = vec![0usize; count];
    for &(_, to) in forward.iter().flatten() {
        incoming[to] += 1;
    }
    let mut ready: Vec<usize> = (0..count).filter(|&n| incoming[n] == 0).collect();
    let mut i = 0;
    while i < ready.len() {
        let node = ready[i];
        i += 1;
        for &(from, to) in forward.iter().flatten() {
            if from == node {
                rank[to] = rank[to].max(rank[node] + 1);
                incoming[to] -= 1;
                if incoming[to] == 0 {
                    ready.push(to);
                }
            }
        }
    }
    let layer_count = rank.iter().max().copied().unwrap_or(0) + 1;

    // 2. Items per layer, with waypoints for links that skip layers.
    let mut items: Vec<Item> = (0..count).map(Item::Node).collect();
    let mut item_layer: Vec<usize> = rank.clone();
    let mut chains: Vec<Vec<usize>> = Vec::new(); // per edge, the items it passes
    for link in &forward {
        let Some((from, to)) = *link else {
            chains.push(Vec::new());
            continue;
        };
        let mut chain = vec![from];
        for layer in rank[from] + 1..rank[to] {
            items.push(Item::Waypoint);
            item_layer.push(layer);
            chain.push(items.len() - 1);
        }
        chain.push(to);
        chains.push(chain);
    }
    let mut layers: Vec<Vec<usize>> = vec![Vec::new(); layer_count];
    for (item, &layer) in item_layer.iter().enumerate() {
        layers[layer].push(item);
    }
    let mut above: Vec<Vec<usize>> = vec![Vec::new(); items.len()];
    let mut below: Vec<Vec<usize>> = vec![Vec::new(); items.len()];
    for chain in &chains {
        for pair in chain.windows(2) {
            below[pair[0]].push(pair[1]);
            above[pair[1]].push(pair[0]);
        }
    }
    let size_r = |item: usize| match items[item] {
        Item::Node(n) => along_r(sizes[n]),
        Item::Waypoint => 0.0,
    };
    let size_o = |item: usize| match items[item] {
        Item::Node(n) => along_o(sizes[n]),
        Item::Waypoint => 0.0,
    };

    // 3. Order inside layers: by the average place of the neighbours,
    // sweeping down and up a few times.
    let mut place = vec![0.0f32; items.len()];
    let renumber = |layers: &Vec<Vec<usize>>, place: &mut Vec<f32>| {
        for layer in layers {
            for (i, &item) in layer.iter().enumerate() {
                place[item] = i as f32;
            }
        }
    };
    renumber(&layers, &mut place);
    for sweep in 0..8 {
        let down = sweep % 2 == 0;
        let order: Vec<usize> = if down { (1..layer_count).collect() } else { (0..layer_count.saturating_sub(1)).rev().collect() };
        for l in order {
            let keys: HashMap<usize, f32> = layers[l]
                .iter()
                .map(|&item| {
                    let neighbours = if down { &above[item] } else { &below[item] };
                    let key = if neighbours.is_empty() {
                        place[item]
                    } else {
                        neighbours.iter().map(|&n| place[n]).sum::<f32>() / neighbours.len() as f32
                    };
                    (item, key)
                })
                .collect();
            layers[l].sort_by(|a, b| keys[a].total_cmp(&keys[b]));
            for (i, &item) in layers[l].iter().enumerate() {
                place[item] = i as f32;
            }
        }
    }

    // 4. Positions across the layers: packed in order, then pulled towards
    // their neighbours, layer by layer.
    let gap_o = if vertical { cell_w * 3.0 } else { cell_h * 1.2 };
    let separation = |a: usize, b: usize| {
        let gap = if matches!(items[a], Item::Waypoint) || matches!(items[b], Item::Waypoint) { gap_o / 2.0 } else { gap_o };
        (size_o(a) + size_o(b)) / 2.0 + gap
    };
    let mut center_o = vec![0.0f32; items.len()];
    for layer in &layers {
        let mut o = 0.0;
        for (i, &item) in layer.iter().enumerate() {
            if i > 0 {
                o += separation(layer[i - 1], item);
            }
            center_o[item] = o;
        }
    }
    for round in 0..6 {
        let down = round % 2 == 0;
        let order: Vec<usize> = if down { (1..layer_count).collect() } else { (0..layer_count.saturating_sub(1)).rev().collect() };
        for l in order {
            let layer = &layers[l];
            let wanted: Vec<f32> = layer
                .iter()
                .map(|&item| {
                    let neighbours = if down { &above[item] } else { &below[item] };
                    if neighbours.is_empty() {
                        center_o[item]
                    } else {
                        neighbours.iter().map(|&n| center_o[n]).sum::<f32>() / neighbours.len() as f32
                    }
                })
                .collect();
            // Keep the order and the gaps, then shift the layer so that on
            // average its items sit where they want to be.
            let mut placed = wanted.clone();
            for i in 1..placed.len() {
                placed[i] = placed[i].max(placed[i - 1] + separation(layer[i - 1], layer[i]));
            }
            let shift = wanted.iter().zip(&placed).map(|(w, p)| w - p).sum::<f32>() / placed.len().max(1) as f32;
            for (i, &item) in layer.iter().enumerate() {
                center_o[item] = placed[i] + shift;
            }
        }
    }

    // 5. Positions along the layers. Gaps grow to fit the labels of the links
    // crossing them.
    let thickness: Vec<f32> = layers.iter().map(|layer| layer.iter().map(|&i| size_r(i)).fold(0.0, f32::max)).collect();
    let label_size = |label: &str| {
        let lines: Vec<&str> = label.lines().collect();
        let chars = lines.iter().map(|l| l.chars().count()).max().unwrap_or(0) as f32;
        (chars * cell_w + cell_w, lines.len() as f32 * cell_h + cell_h * 0.4)
    };
    let base_gap = if vertical { cell_h * 2.5 } else { cell_w * 6.0 };
    let mut gaps = vec![base_gap; layer_count.saturating_sub(1)];
    for (edge, chain) in graph.edges.iter().zip(&chains) {
        if let (Some(label), Some(&first)) = (&edge.label, chain.first())
            && let Some(gap) = gaps.get_mut(item_layer[first])
        {
            let along = along_r(label_size(label));
            *gap = gap.max(along + if vertical { cell_h * 1.5 } else { cell_w * 3.0 });
        }
    }
    let margin = cell_w * 2.0;
    let mut layer_start = vec![margin; layer_count];
    for l in 1..layer_count {
        layer_start[l] = layer_start[l - 1] + thickness[l - 1] + gaps[l - 1];
    }
    let center_r = |item: usize| layer_start[item_layer[item]] + thickness[item_layer[item]] / 2.0;
    // Waypoints span their whole layer, so lines pass straight through it.
    let half_r = |item: usize| match items[item] {
        Item::Node(_) => size_r(item) / 2.0,
        Item::Waypoint => thickness[item_layer[item]] / 2.0,
    };

    // 6. Link paths, in (r, o). Links crossing the same gap get their own
    // lane in it, so their across-lines do not lie on top of each other.
    let mut lanes: Vec<Vec<usize>> = vec![Vec::new(); layer_count];
    for (e, chain) in chains.iter().enumerate() {
        for &item in chain.iter().take(chain.len().saturating_sub(1)) {
            lanes[item_layer[item]].push(e);
        }
    }
    let lane_r = |layer: usize, edge: usize| {
        let lane = &lanes[layer];
        let k = lane.iter().position(|&e| e == edge).unwrap_or(0);
        let start = layer_start[layer] + thickness[layer];
        start + gaps.get(layer).copied().unwrap_or(base_gap) * (k + 1) as f32 / (lane.len() + 1) as f32
    };
    let mut paths: Vec<Vec<(f32, f32)>> = Vec::new();
    let mut label_at: Vec<Option<(f32, f32)>> = Vec::new();
    for (e, chain) in chains.iter().enumerate() {
        let edge = &graph.edges[e];
        if chain.is_empty() {
            // A node linked to itself: a small loop on its far side.
            let n = edge.from;
            let (r, o) = (center_r(n), center_o[n] + size_o(n) / 2.0);
            let (dr, d_o) = (size_r(n) / 4.0, if vertical { cell_w * 2.0 } else { cell_h });
            paths.push(vec![(r - dr, o), (r - dr, o + d_o), (r + dr, o + d_o), (r + dr, o)]);
            label_at.push(Some((r, o + d_o * 1.5)));
            continue;
        }
        let mut points = vec![(center_r(chain[0]) + half_r(chain[0]), center_o[chain[0]])];
        let mut label = None;
        for pair in chain.windows(2) {
            let (a, b) = (pair[0], pair[1]);
            let lane = lane_r(item_layer[a], e);
            points.push((lane, center_o[a]));
            points.push((lane, center_o[b]));
            if label.is_none() {
                label = Some((lane, (center_o[a] + center_o[b]) / 2.0));
            }
            points.push((center_r(b) - half_r(b), center_o[b]));
            if matches!(items[b], Item::Waypoint) {
                points.push((center_r(b) + half_r(b), center_o[b]));
            }
        }
        if reversed[e] {
            points.reverse();
        }
        paths.push(simplify(points));
        label_at.push(label);
    }

    // 7. From (r, o) to pixels, turned to the diagram's direction.
    let extent_r = layer_start[layer_count - 1] + thickness[layer_count - 1] + margin;
    let min_o = (0..items.len()).map(|i| center_o[i] - size_o(i) / 2.0).fold(f32::MAX, f32::min);
    let to_xy = |r: f32, o: f32| -> (f32, f32) {
        let o = o - min_o + margin;
        match graph.direction {
            Direction::Down => (o, r),
            Direction::Up => (o, extent_r - r),
            Direction::Right => (r, o),
            Direction::Left => (extent_r - r, o),
        }
    };
    let nodes: Vec<NodeBox> = graph
        .nodes
        .iter()
        .enumerate()
        .map(|(n, node)| {
            let (cx, cy) = to_xy(center_r(n), center_o[n]);
            let (w, h) = sizes[n];
            NodeBox { shape: node.shape, x: cx - w / 2.0, y: cy - h / 2.0, w, h, lines: node.label.lines().map(str::to_string).collect() }
        })
        .collect();
    let edges: Vec<EdgeLine> = graph
        .edges
        .iter()
        .enumerate()
        .map(|(e, edge)| EdgeLine {
            points: paths[e].iter().map(|&(r, o)| to_xy(r, o)).collect(),
            stroke: edge.stroke,
            start: edge.start,
            end: edge.end,
            label: edge.label.as_ref().zip(label_at[e]).map(|(text, (r, o))| {
                let (x, y) = to_xy(r, o);
                (text.lines().map(str::to_string).collect(), x, y)
            }),
        })
        .collect();

    // 8. Subgraphs: around their nodes and the subgraphs inside them, with
    // the title on top. Inner ones are measured first.
    let pad = cell_w;
    let mut boxes: Vec<Option<(f32, f32, f32, f32)>> = vec![None; graph.groups.len()];
    for g in (0..graph.groups.len()).rev() {
        let mut bounds: Option<(f32, f32, f32, f32)> = None;
        let mut add = |x0: f32, y0: f32, x1: f32, y1: f32| {
            bounds = Some(match bounds {
                Some((a, b, c, d)) => (a.min(x0), b.min(y0), c.max(x1), d.max(y1)),
                None => (x0, y0, x1, y1),
            });
        };
        for &n in &graph.groups[g].members {
            let node = &nodes[n];
            add(node.x, node.y, node.x + node.w, node.y + node.h);
        }
        for (child, group) in graph.groups.iter().enumerate() {
            if group.parent == Some(g)
                && let Some((x0, y0, x1, y1)) = boxes[child]
            {
                add(x0, y0, x1, y1);
            }
        }
        boxes[g] = bounds.map(|(x0, y0, x1, y1)| (x0 - pad, y0 - pad - cell_h * 1.2, x1 + pad, y1 + pad));
    }
    let groups: Vec<GroupBox> = graph
        .groups
        .iter()
        .zip(&boxes)
        .filter_map(|(group, b)| b.map(|(x0, y0, x1, y1)| GroupBox { x: x0, y: y0, w: x1 - x0, h: y1 - y0, title: group.title.clone() }))
        .collect();

    let mut diagram = Diagram { width: 0.0, height: 0.0, groups, edges, nodes };
    fit(&mut diagram, margin);
    diagram
}

/// Drop points that do not turn the path.
fn simplify(points: Vec<(f32, f32)>) -> Vec<(f32, f32)> {
    let mut out: Vec<(f32, f32)> = Vec::new();
    for p in points {
        if out.last().is_some_and(|q| (q.0 - p.0).abs() < 0.01 && (q.1 - p.1).abs() < 0.01) {
            continue;
        }
        if out.len() >= 2 {
            let (a, b) = (out[out.len() - 2], out[out.len() - 1]);
            let straight = ((a.0 - b.0).abs() < 0.01 && (b.0 - p.0).abs() < 0.01) || ((a.1 - b.1).abs() < 0.01 && (b.1 - p.1).abs() < 0.01);
            if straight {
                out.pop();
            }
        }
        out.push(p);
    }
    out
}

/// Move everything so the top-left corner is `margin` away from the
/// origin, and size the diagram to hold it all.
fn fit(diagram: &mut Diagram, margin: f32) {
    let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    let mut add = |x: f32, y: f32| {
        x0 = x0.min(x);
        y0 = y0.min(y);
        x1 = x1.max(x);
        y1 = y1.max(y);
    };
    for n in &diagram.nodes {
        add(n.x, n.y);
        add(n.x + n.w, n.y + n.h);
    }
    for g in &diagram.groups {
        add(g.x, g.y);
        add(g.x + g.w, g.y + g.h);
    }
    for e in &diagram.edges {
        for &(x, y) in &e.points {
            add(x, y);
        }
    }
    let (dx, dy) = (margin - x0, margin - y0);
    for n in &mut diagram.nodes {
        n.x += dx;
        n.y += dy;
    }
    for g in &mut diagram.groups {
        g.x += dx;
        g.y += dy;
    }
    for e in &mut diagram.edges {
        for p in &mut e.points {
            p.0 += dx;
            p.1 += dy;
        }
        if let Some((_, x, y)) = &mut e.label {
            *x += dx;
            *y += dy;
        }
    }
    diagram.width = (x1 - x0 + margin * 2.0).ceil();
    diagram.height = (y1 - y0 + margin * 2.0).ceil();
}

// ---------------------------------------------------------------------------
// Shapes
// ---------------------------------------------------------------------------

/// Corners of a polygon, or the points of a line, in pixels.
pub type Points = Vec<(f32, f32)>;

/// A node's outline as a polygon, plus extra lines drawn inside it (the
/// sides of a subroutine, the rim of a database).
pub fn outline(node: &NodeBox) -> (Points, Vec<Points>) {
    let NodeBox { x, y, w, h, .. } = *node;
    let (right, bottom) = (x + w, y + h);
    let arc = |cx: f32, cy: f32, rx: f32, ry: f32, from: f32, to: f32, steps: usize| -> Vec<(f32, f32)> {
        (0..=steps)
            .map(|i| {
                let a = (from + (to - from) * i as f32 / steps as f32).to_radians();
                (cx + rx * a.cos(), cy + ry * a.sin())
            })
            .collect()
    };
    match node.shape {
        Shape::Rect | Shape::Subroutine => {
            let corners = vec![(x, y), (right, y), (right, bottom), (x, bottom)];
            let extra = if node.shape == Shape::Subroutine {
                let inset = (h * 0.2).min(w / 4.0);
                vec![vec![(x + inset, y), (x + inset, bottom)], vec![(right - inset, y), (right - inset, bottom)]]
            } else {
                Vec::new()
            };
            (corners, extra)
        }
        Shape::Round => {
            let r = (h * 0.25).min(w / 2.0);
            let mut points = arc(right - r, y + r, r, r, 270.0, 360.0, 4);
            points.extend(arc(right - r, bottom - r, r, r, 0.0, 90.0, 4));
            points.extend(arc(x + r, bottom - r, r, r, 90.0, 180.0, 4));
            points.extend(arc(x + r, y + r, r, r, 180.0, 270.0, 4));
            (points, Vec::new())
        }
        Shape::Stadium => {
            let r = h / 2.0;
            let mut points = arc(right - r, y + r, r, r, 270.0, 450.0, 10);
            points.extend(arc(x + r, y + r, r, r, 90.0, 270.0, 10));
            (points, Vec::new())
        }
        Shape::Circle => (arc(x + w / 2.0, y + h / 2.0, w / 2.0, h / 2.0, 0.0, 360.0, 32), Vec::new()),
        Shape::Diamond => (vec![(x + w / 2.0, y), (right, y + h / 2.0), (x + w / 2.0, bottom), (x, y + h / 2.0)], Vec::new()),
        Shape::Hexagon => {
            let i = h / 2.0;
            (vec![(x + i, y), (right - i, y), (right, y + h / 2.0), (right - i, bottom), (x + i, bottom), (x, y + h / 2.0)], Vec::new())
        }
        Shape::Flag => (vec![(x, y), (right, y), (right, bottom), (x, bottom), (x + h / 2.0, y + h / 2.0)], Vec::new()),
        Shape::Lean => (vec![(x + h / 2.0, y), (right, y), (right - h / 2.0, bottom), (x, bottom)], Vec::new()),
        Shape::LeanLeft => (vec![(x, y), (right - h / 2.0, y), (right, bottom), (x + h / 2.0, bottom)], Vec::new()),
        Shape::Trapezoid => (vec![(x + h / 2.0, y), (right - h / 2.0, y), (right, bottom), (x, bottom)], Vec::new()),
        Shape::TrapezoidDown => (vec![(x, y), (right, y), (right - h / 2.0, bottom), (x + h / 2.0, bottom)], Vec::new()),
        Shape::Cylinder => {
            let ry = (h * 0.12).max(3.0);
            let rx = w / 2.0;
            let cx = x + rx;
            let mut points = arc(cx, y + ry, rx, ry, 180.0, 360.0, 12);
            points.extend(arc(cx, bottom - ry, rx, ry, 0.0, 180.0, 12));
            let rim = arc(cx, y + ry, rx, ry, 180.0, 0.0, 12);
            (points, vec![rim])
        }
    }
}

/// The triangle of an arrow head whose tip is at `tip`, coming from `from`.
pub fn arrow_head(tip: (f32, f32), from: (f32, f32), size: f32) -> Vec<(f32, f32)> {
    let (dx, dy) = (tip.0 - from.0, tip.1 - from.1);
    let length = (dx * dx + dy * dy).sqrt().max(0.001);
    let (ux, uy) = (dx / length, dy / length);
    let back = (tip.0 - ux * size, tip.1 - uy * size);
    let half = size * 0.5;
    vec![tip, (back.0 - uy * half, back.1 + ux * half), (back.0 + uy * half, back.1 - ux * half)]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_nodes_links_and_subgraphs() {
        let source = "flowchart LR
    %% a comment
    A[Start here] --> B{Is it?}
    B -- Yes --> C([Done]) & D[(Store)]
    B -->|No| E((Retry)) -.-> A
    subgraph sub [The group]
      F>flag] ==> G[/lean/]
    end
    G --o H; H --x I:::warm
    style A fill:#f9f";
        let graph = parse(source).unwrap();
        assert_eq!(graph.direction, Direction::Right);
        let names: Vec<&str> = graph.nodes.iter().map(|n| n.id.as_str()).collect();
        assert_eq!(names, vec!["A", "B", "C", "D", "E", "F", "G", "H", "I"]);
        assert_eq!(graph.nodes[1].shape, Shape::Diamond);
        assert_eq!(graph.nodes[1].label, "Is it?");
        assert_eq!(graph.nodes[2].shape, Shape::Stadium);
        assert_eq!(graph.nodes[3].shape, Shape::Cylinder);
        assert_eq!(graph.nodes[4].shape, Shape::Circle);
        assert_eq!(graph.nodes[5].shape, Shape::Flag);
        assert_eq!(graph.nodes[6].shape, Shape::Lean);
        let link = |from: &str, to: &str| {
            let index = |id: &str| names.iter().position(|n| *n == id).unwrap();
            graph.edges.iter().find(|e| e.from == index(from) && e.to == index(to)).unwrap_or_else(|| panic!("{from} -> {to}"))
        };
        assert_eq!(link("B", "C").label.as_deref(), Some("Yes"));
        assert_eq!(link("B", "D").label.as_deref(), Some("Yes"), "`&` links to both");
        assert_eq!(link("B", "E").label.as_deref(), Some("No"));
        assert_eq!(link("E", "A").stroke, Stroke::Dotted);
        assert_eq!(link("F", "G").stroke, Stroke::Thick);
        assert_eq!(link("G", "H").end, Head::Circle);
        assert_eq!(link("H", "I").end, Head::Cross);
        assert_eq!(link("A", "B").end, Head::Arrow);
        assert_eq!(graph.groups[0].title, "The group");
        assert_eq!(graph.groups[0].members, vec![5, 6]);
    }

    #[test]
    fn labels_and_odd_ids() {
        let graph = parse("graph TD\nnode-1[\"Quoted [label]<br>two\"] --- xray\n").unwrap();
        assert_eq!(graph.nodes[0].id, "node-1");
        assert_eq!(graph.nodes[0].label, "Quoted [label]\ntwo");
        assert_eq!(graph.nodes[1].id, "xray", "the x of `--- xray` is not a head");
        assert_eq!(graph.edges[0].end, Head::None);
        assert!(parse("sequenceDiagram\nA->>B: hi").unwrap_err().contains("only flowcharts"));
        assert!(parse("flowchart TD\nA --> \n").is_err());
        assert!(parse("flowchart TD\nsubgraph x\nA\n").is_err());
    }

    #[test]
    fn odd_and_half_typed_sources_never_panic() {
        for source in ["graph TD\nA", "flowchart TD\nA-->A", "flowchart RL\nA-->B\nB-->A\nA-->C-->D-->A"] {
            assert!(render(source, 8.0, 16.0).is_ok(), "{source}");
        }
        assert!(render("graph BT\nsubgraph s\nend", 8.0, 16.0).is_err(), "nothing to draw");
        let long = "flowchart TD\n  A[\"x\"] -- a b --> B{y} & C((z))\n  B -.->|t| D[(d)] ==> A\n  subgraph g [G]\n    C --o E>e] --x F[/f\\]\n  end\n";
        for cut in 0..long.len() {
            if long.is_char_boundary(cut) {
                let _ = render(&long[..cut], 8.0, 16.0);
            }
        }
    }

    #[test]
    fn layers_go_down_and_links_end_at_their_nodes() {
        let diagram = render("flowchart TD\nA --> B --> C\nA --> C\nC --> A\n", 8.0, 16.0).unwrap();
        let [a, b, c] = [&diagram.nodes[0], &diagram.nodes[1], &diagram.nodes[2]];
        assert!(a.y + a.h < b.y && b.y + b.h < c.y, "one layer each, top to bottom");
        // A --> C skips B's layer: it goes around B, not through it.
        let skip = &diagram.edges[2];
        for pair in skip.points.windows(2) {
            let (x0, x1) = (pair[0].0.min(pair[1].0), pair[0].0.max(pair[1].0));
            let (y0, y1) = (pair[0].1.min(pair[1].1), pair[0].1.max(pair[1].1));
            let crosses = x1 > b.x && x0 < b.x + b.w && y1 > b.y && y0 < b.y + b.h;
            assert!(!crosses, "segment {pair:?} crosses B");
        }
        // The cycle C --> A still points at A: it ends on A's bottom edge.
        let back = &diagram.edges[3];
        let end = *back.points.last().unwrap();
        assert!((end.1 - (a.y + a.h)).abs() < 0.5, "{end:?} vs {a:?}");
        assert!(diagram.width > 0.0 && diagram.height > c.y + c.h);
        let lr = render("graph LR\nA --> B\n", 8.0, 16.0).unwrap();
        assert!(lr.nodes[0].x + lr.nodes[0].w < lr.nodes[1].x, "left to right");
    }
}
