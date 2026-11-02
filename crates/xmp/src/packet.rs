//! Generic RDF/XMP data model: a packet is a list of properties, each
//! identified by namespace URI + local name. Values follow the XMP data
//! model (simple text, ordered/unordered arrays, language alternatives,
//! structs), which is everything Camera Raw presets use.

use std::fmt::Write as _;

use crate::Error;

pub const NS_RDF: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";
pub const NS_XML: &str = "http://www.w3.org/XML/1998/namespace";
pub const NS_CRS: &str = "http://ns.adobe.com/camera-raw-settings/1.0/";

/// Prefixes used when writing a namespace the source packet did not declare.
const WELL_KNOWN: &[(&str, &str)] = &[
    ("crs", NS_CRS),
    ("crd", "http://ns.adobe.com/camera-raw-defaults/1.0/"),
    ("xmp", "http://ns.adobe.com/xap/1.0/"),
    ("tiff", "http://ns.adobe.com/tiff/1.0/"),
    ("exif", "http://ns.adobe.com/exif/1.0/"),
    ("aux", "http://ns.adobe.com/exif/1.0/aux/"),
    ("dc", "http://purl.org/dc/elements/1.1/"),
    ("photoshop", "http://ns.adobe.com/photoshop/1.0/"),
    ("xmpMM", "http://ns.adobe.com/xap/1.0/mm/"),
];

#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Text(String),
    Seq(Vec<Value>),
    Bag(Vec<Value>),
    /// Language alternative: (xml:lang, value).
    Alt(Vec<(Option<String>, Value)>),
    Struct(Props),
}

impl Value {
    pub fn as_text(&self) -> Option<&str> {
        match self {
            Value::Text(t) => Some(t),
            // A lang-alt is usually read for its default entry.
            Value::Alt(items) => items
                .iter()
                .find(|(l, _)| l.as_deref() == Some("x-default"))
                .or_else(|| items.first())
                .and_then(|(_, v)| v.as_text()),
            _ => None,
        }
    }

    /// Numbers are stored as text, often with an explicit sign ("+50").
    pub fn as_f32(&self) -> Option<f32> {
        self.as_text()?.trim().trim_start_matches('+').parse().ok()
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self.as_text()?.trim() {
            t if t.eq_ignore_ascii_case("true") => Some(true),
            t if t.eq_ignore_ascii_case("false") => Some(false),
            _ => None,
        }
    }

    pub fn items(&self) -> &[Value] {
        match self {
            Value::Seq(v) | Value::Bag(v) => v,
            _ => &[],
        }
    }

    pub fn as_struct(&self) -> Option<&Props> {
        match self {
            Value::Struct(p) => Some(p),
            _ => None,
        }
    }

    pub fn lang_alt(text: impl Into<String>) -> Value {
        Value::Alt(vec![(Some("x-default".into()), Value::Text(text.into()))])
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Prop {
    pub ns: String,
    pub name: String,
    pub value: Value,
}

/// Ordered property list. Order is kept so a parse/write round trip
/// produces a file that diffs cleanly against the original.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Props(pub Vec<Prop>);

impl Props {
    pub fn get(&self, ns: &str, name: &str) -> Option<&Value> {
        self.0.iter().find(|p| p.ns == ns && p.name == name).map(|p| &p.value)
    }

    pub fn crs(&self, name: &str) -> Option<&Value> {
        self.get(NS_CRS, name)
    }

    pub fn set(&mut self, ns: &str, name: &str, value: Value) {
        match self.0.iter_mut().find(|p| p.ns == ns && p.name == name) {
            Some(p) => p.value = value,
            None => self.0.push(Prop { ns: ns.into(), name: name.into(), value }),
        }
    }

    pub fn set_crs(&mut self, name: &str, value: Value) {
        self.set(NS_CRS, name, value)
    }

    pub fn remove(&mut self, ns: &str, name: &str) -> Option<Value> {
        let i = self.0.iter().position(|p| p.ns == ns && p.name == name)?;
        Some(self.0.remove(i).value)
    }

    pub fn iter(&self) -> impl Iterator<Item = &Prop> {
        self.0.iter()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct XmpPacket {
    /// (prefix, uri) pairs declared in the source document.
    pub prefixes: Vec<(String, String)>,
    pub props: Props,
}

impl XmpPacket {
    pub fn parse(xml: &str) -> Result<Self, Error> {
        let opts = roxmltree::ParsingOptions { allow_dtd: true, ..Default::default() };
        let doc = roxmltree::Document::parse_with_options(xml, opts)
            .map_err(|e| Error::Xml(e.to_string()))?;

        let mut packet = XmpPacket::default();
        for node in doc.descendants().filter(|n| n.is_element()) {
            for ns in node.namespaces() {
                if let Some(prefix) = ns.name() {
                    if !packet.prefixes.iter().any(|(_, u)| u == ns.uri()) {
                        packet.prefixes.push((prefix.to_string(), ns.uri().to_string()));
                    }
                }
            }
        }

        let rdf = doc
            .descendants()
            .find(|n| is_rdf(n, "RDF"))
            .ok_or(Error::NoXmp)?;
        // Writers may split properties across several top-level
        // descriptions; they all describe the same resource.
        for desc in rdf.children().filter(|n| is_rdf(n, "Description")) {
            let props = parse_resource(desc);
            for p in props.0 {
                packet.props.set(&p.ns, &p.name, p.value);
            }
        }
        Ok(packet)
    }

    pub fn to_xml(&self) -> String {
        let mut uris = Vec::new();
        collect_namespaces(&self.props, &mut uris);
        let prefixes: Vec<(String, String)> = uris
            .iter()
            .enumerate()
            .map(|(i, uri)| (self.prefix_for(uri, i), uri.clone()))
            .collect();

        let mut w = Writer { out: String::new(), prefixes: &prefixes };
        w.out.push_str("<x:xmpmeta xmlns:x=\"adobe:ns:meta/\" x:xmptk=\"Lumen XMP 0.1\">\n");
        w.out.push_str(" <rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\">\n");
        w.description(&self.props, 2, true);
        w.out.push_str(" </rdf:RDF>\n</x:xmpmeta>\n");
        w.out
    }

    fn prefix_for(&self, uri: &str, index: usize) -> String {
        self.prefixes
            .iter()
            .map(|(p, u)| (p.as_str(), u.as_str()))
            .chain(WELL_KNOWN.iter().copied())
            .find(|(_, u)| *u == uri)
            .map(|(p, _)| p.to_string())
            .unwrap_or_else(|| format!("ns{index}"))
    }
}

fn is_rdf(node: &roxmltree::Node, local: &str) -> bool {
    node.is_element() && node.tag_name().namespace() == Some(NS_RDF) && node.tag_name().name() == local
}

fn rdf_attr<'a>(node: &roxmltree::Node<'a, '_>, local: &str) -> Option<&'a str> {
    node.attribute((NS_RDF, local))
}

/// Attributes (except rdf:* / xml:*) and child elements of a resource node
/// become its properties.
fn parse_resource(node: roxmltree::Node) -> Props {
    let mut props = Props::default();
    for attr in node.attributes() {
        let Some(ns) = attr.namespace() else { continue };
        if ns == NS_RDF || ns == NS_XML {
            continue;
        }
        props.0.push(Prop { ns: ns.into(), name: attr.name().into(), value: Value::Text(attr.value().into()) });
    }
    for child in node.children().filter(|n| n.is_element()) {
        let ns = child.tag_name().namespace().unwrap_or("").to_string();
        props.0.push(Prop { ns, name: child.tag_name().name().into(), value: parse_property(child) });
    }
    props
}

fn parse_property(node: roxmltree::Node) -> Value {
    if rdf_attr(&node, "parseType") == Some("Resource") {
        return Value::Struct(parse_resource(node));
    }
    if let Some(res) = rdf_attr(&node, "resource") {
        return Value::Text(res.into());
    }
    let elems: Vec<_> = node.children().filter(|n| n.is_element()).collect();
    match elems.as_slice() {
        [] => {
            // Property attributes on an empty element are struct shorthand
            // (e.g. <crs:LensBlur crs:Version="1" .../>).
            let has_fields = node
                .attributes()
                .any(|a| matches!(a.namespace(), Some(ns) if ns != NS_RDF && ns != NS_XML));
            if has_fields {
                Value::Struct(parse_resource(node))
            } else {
                Value::Text(node.text().unwrap_or("").to_string())
            }
        }
        [one] if is_rdf(one, "Seq") => Value::Seq(list_items(*one).map(|(_, v)| v).collect()),
        [one] if is_rdf(one, "Bag") => Value::Bag(list_items(*one).map(|(_, v)| v).collect()),
        [one] if is_rdf(one, "Alt") => Value::Alt(list_items(*one).collect()),
        [one] if is_rdf(one, "Description") => Value::Struct(parse_resource(*one)),
        _ => Value::Struct(parse_resource(node)),
    }
}

fn list_items<'a>(list: roxmltree::Node<'a, 'a>) -> impl Iterator<Item = (Option<String>, Value)> + 'a {
    list.children().filter(|n| is_rdf(n, "li")).map(|li| {
        let lang = li.attribute((NS_XML, "lang")).map(str::to_string);
        (lang, parse_property(li))
    })
}

fn collect_namespaces(props: &Props, out: &mut Vec<String>) {
    for p in props.iter() {
        if !out.contains(&p.ns) {
            out.push(p.ns.clone());
        }
        collect_value_namespaces(&p.value, out);
    }
}

fn collect_value_namespaces(v: &Value, out: &mut Vec<String>) {
    match v {
        Value::Text(_) => {}
        Value::Seq(items) | Value::Bag(items) => items.iter().for_each(|i| collect_value_namespaces(i, out)),
        Value::Alt(items) => items.iter().for_each(|(_, i)| collect_value_namespaces(i, out)),
        Value::Struct(p) => collect_namespaces(p, out),
    }
}

struct Writer<'a> {
    out: String,
    prefixes: &'a [(String, String)],
}

impl Writer<'_> {
    fn qname(&self, ns: &str, name: &str) -> String {
        let prefix = self.prefixes.iter().find(|(_, u)| u == ns).map(|(p, _)| p.as_str()).unwrap_or("ns");
        format!("{prefix}:{name}")
    }

    /// Simple text fields go into attributes, everything else into child
    /// elements, the same layout Camera Raw writes.
    fn description(&mut self, props: &Props, depth: usize, top: bool) {
        let pad = " ".repeat(depth);
        self.out.push_str(&pad);
        self.out.push_str("<rdf:Description");
        if top {
            self.out.push_str(" rdf:about=\"\"");
            for (prefix, uri) in self.prefixes {
                let _ = write!(self.out, "\n{pad}  xmlns:{prefix}=\"{}\"", escape(uri));
            }
        }
        for p in props.iter() {
            if let Value::Text(t) = &p.value {
                let _ = write!(self.out, "\n{pad} {}=\"{}\"", self.qname(&p.ns, &p.name), escape(t));
            }
        }
        let complex: Vec<_> = props.iter().filter(|p| !matches!(p.value, Value::Text(_))).collect();
        if complex.is_empty() {
            self.out.push_str("/>\n");
            return;
        }
        self.out.push_str(">\n");
        for p in complex {
            let q = self.qname(&p.ns, &p.name);
            self.element(&q, None, &p.value, depth + 1);
        }
        let _ = writeln!(self.out, "{pad}</rdf:Description>");
    }

    fn element(&mut self, qname: &str, lang: Option<&str>, value: &Value, depth: usize) {
        let pad = " ".repeat(depth);
        let lang_attr = lang.map(|l| format!(" xml:lang=\"{}\"", escape(l))).unwrap_or_default();
        match value {
            Value::Text(t) => {
                let _ = writeln!(self.out, "{pad}<{qname}{lang_attr}>{}</{qname}>", escape(t));
            }
            Value::Seq(items) | Value::Bag(items) => {
                let kind = if matches!(value, Value::Seq(_)) { "rdf:Seq" } else { "rdf:Bag" };
                let _ = writeln!(self.out, "{pad}<{qname}{lang_attr}>\n{pad} <{kind}>");
                for item in items {
                    self.element("rdf:li", None, item, depth + 2);
                }
                let _ = writeln!(self.out, "{pad} </{kind}>\n{pad}</{qname}>");
            }
            Value::Alt(items) => {
                let _ = writeln!(self.out, "{pad}<{qname}{lang_attr}>\n{pad} <rdf:Alt>");
                for (l, item) in items {
                    self.element("rdf:li", l.as_deref(), item, depth + 2);
                }
                let _ = writeln!(self.out, "{pad} </rdf:Alt>\n{pad}</{qname}>");
            }
            Value::Struct(props) => {
                let _ = writeln!(self.out, "{pad}<{qname}{lang_attr}>");
                self.description(props, depth + 1, false);
                let _ = writeln!(self.out, "{pad}</{qname}>");
            }
        }
    }
}

fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\n' => out.push_str("&#xA;"),
            c => out.push(c),
        }
    }
    out
}
