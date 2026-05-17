use crate::parser::cst::{EncodedNode, Heading, MdLink, MdLinkDef, Tag};

pub type AstIdx = usize;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AstElement {
    H(Heading),
    WL(crate::parser::cst::WikiLink),
    ML(MdLink),
    MLD(MdLinkDef),
    T(Tag),
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Ast {
    pub elements: Vec<AstElement>,
}

pub fn decode_node(node: &EncodedNode) -> String {
    node.decoded.clone()
}
