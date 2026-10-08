//! Abstract syntax tree for `.blog` source text.
//!
//! These types mirror the surface syntax before lowering into a
//! [`BlockGraph`](crate::BlockGraph): references are still unresolved (block ids
//! or position literals) and every node carries a source [`Span`] for
//! diagnostics.

use crate::{Basis, BlockKind, CubeHeight, Direction, UDirection, WalkingBoundaryKind};
use bloq_utils::PauliBasis;
use glam::IVec3;

use crate::action::BinaryOp;

/// Byte-offset span in source text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    /// Inclusive starting byte offset.
    pub start: u32,
    /// Exclusive ending byte offset.
    pub end: u32,
}

impl Span {
    /// Converts a `usize` byte range, saturating offsets above [`u32::MAX`].
    pub fn from_range(range: std::ops::Range<usize>) -> Self {
        // Saturate rather than silently wrap on the (practically impossible)
        // >4 GiB source file, so a span never points at a bogus offset.
        Self {
            start: u32::try_from(range.start).unwrap_or(u32::MAX),
            end: u32::try_from(range.end).unwrap_or(u32::MAX),
        }
    }
}

/// Value with source location.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Spanned<T> {
    /// Parsed value.
    pub node: T,
    /// Source range containing the value.
    pub span: Span,
}

impl<T> Spanned<T> {
    /// Wraps a parsed value with its source span.
    pub fn new(node: T, span: Span) -> Self {
        Self { node, span }
    }
}

/// Standalone BLOG source grouped by statement kind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceFile {
    /// Declared BLOG major and minor version.
    pub version: Spanned<(u32, u32)>,
    /// Graph-geometry statements in source order.
    pub data_stmts: Vec<Spanned<DataStmt>>,
    /// Classical-action statements in source order.
    pub action_stmts: Vec<Spanned<ActionStmt>>,
}

/// A BLOG file containing named module definitions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModularSourceFile {
    /// Declared BLOG major and minor version.
    pub version: Spanned<(u32, u32)>,
    /// Imported module sources.
    pub imports: Vec<Spanned<ImportDef>>,
    /// Module definitions in source order.
    pub modules: Vec<Spanned<ModuleDef>>,
}

/// `import "path" as name`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportDef {
    /// Imported source path.
    pub path: Spanned<String>,
    /// Local name assigned to the imported definition.
    pub alias: Spanned<String>,
}

/// `module name { ... }`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModuleDef {
    /// Module definition name.
    pub name: Spanned<String>,
    /// Declared quantum and classical interface.
    pub interface_stmts: Vec<Spanned<InterfaceStmt>>,
    /// Child module instances.
    pub instance_stmts: Vec<Spanned<InstanceDef>>,
    /// Parent-local graph geometry.
    pub data_stmts: Vec<Spanned<DataStmt>>,
    /// Parent-local classical actions.
    pub action_stmts: Vec<Spanned<ActionStmt>>,
    /// Connections between blocks, instances, and interface ports.
    pub connect_stmts: Vec<Spanned<ConnectStmt>>,
}

/// A module interface statement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InterfaceStmt {
    /// Quantum input or output port.
    Quantum(QuantumPortDef),
    /// Named classical bit input.
    BitInput(Spanned<String>),
    /// Named classical bit output expression.
    BitOutput(BitOutputDef),
}

/// `in|out name: resource-type = block-id`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuantumPortDef {
    /// Whether the port is an input or output.
    pub direction: Spanned<PortDirectionAst>,
    /// Public interface port name.
    pub name: Spanned<String>,
    /// Parent-local block ID implementing the port.
    pub block_id: Spanned<u32>,
    /// Declared logical resource type.
    pub resource_type: Spanned<String>,
}

/// Direction of one declared quantum module port.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PortDirectionAst {
    /// Quantum input port.
    Input,
    /// Quantum output port.
    Output,
}

/// `out name = expr`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BitOutputDef {
    /// Public classical output name.
    pub name: Spanned<String>,
    /// Boolean expression exported through the output.
    pub expr: Spanned<Expr>,
}

/// `name: definition @ [x, y, z] [rotate X|Y|Z degrees]`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstanceDef {
    /// Parent-local instance name.
    pub name: Spanned<String>,
    /// Referenced module definition name.
    pub definition: Spanned<String>,
    /// Translation applied to the instance.
    pub translation: Spanned<IVec3>,
    /// Optional axis and angle applied after translation.
    pub rotation: Option<Spanned<(UDirection, i32)>>,
}

/// A module connection statement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectStmt {
    /// Binds a block endpoint to a named module or interface endpoint.
    Bind {
        /// Whether the connection changes basis by Hadamard.
        hadamard: bool,
        /// Connection source.
        source: Spanned<ConnectEndpointAst>,
        /// Connection target.
        target: Spanned<ConnectEndpointAst>,
    },
    /// Connects a named quantum output to a named quantum input.
    Pipe {
        /// Whether the connection changes basis by Hadamard.
        hadamard: bool,
        /// Qualified output endpoint name.
        output: Spanned<String>,
        /// Qualified input endpoint name.
        input: Spanned<String>,
    },
}

/// A parent-local block ID or named connection endpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectEndpointAst {
    /// Parent-local block ID.
    Block(u32),
    /// Named module or interface endpoint.
    Name(String),
}

/// A graph statement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DataStmt {
    /// Block declaration.
    Block(BlockDef),
    /// Pipe declaration.
    Pipe(PipeDef),
    /// Explicit conditional branch region.
    Branch(BranchRegionDef),
}

/// An action statement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActionStmt {
    /// Named measurement record.
    Measure(MeasureDef),
    /// Named Boolean binding.
    Let(LetDef),
    /// Post-selection condition.
    DiscardIf(Spanned<Expr>),
    /// Selective or branch resolution.
    Resolve(ResolveDef),
    /// Conditional or unconditional Pauli feedback.
    Feedback(FeedbackDef),
}

/// A block or pipe inside one explicit branch arm.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BranchArmStmt {
    /// Block owned by the arm.
    Block(BlockDef),
    /// Pipe owned by the arm.
    Pipe(PipeDef),
}

/// `branch <name> { false { ... } true { ... } }`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BranchRegionDef {
    /// Public branch name.
    pub name: Spanned<String>,
    /// Statements selected when the condition is false.
    pub on_false: Vec<Spanned<BranchArmStmt>>,
    /// Statements selected when the condition is true.
    pub on_true: Vec<Spanned<BranchArmStmt>>,
}

/// Unresolved reference — block ID or position literal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ref {
    /// Explicit lattice position.
    Pos(IVec3),
    /// Source-local block ID.
    Id(u32),
}

/// A block definition: `<id>: <kind> <pos> [...]`.
///
/// `end` is present for multi-cell blocks (walking, patch rotation). `height` is
/// `Some` only when the source wrote a `height=` modifier — the distinction is
/// load-bearing: an explicit height propagates across the cube's spatial
/// component and conflicts with a different explicit height, whereas an absent
/// one silently defaults to `height=d` and never conflicts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockDef {
    /// Source-local block ID.
    pub id: Spanned<u32>,
    /// Parsed block kind before endpoint-dependent lowering.
    pub kind: Spanned<BlockKindAst>,
    /// Whether a selective block's written basis order is reversed.
    pub selective_order_reversed: bool,
    /// Block anchor position.
    pub pos: Spanned<IVec3>,
    /// Explicit symbolic cube height, if written.
    pub height: Option<Spanned<CubeHeight>>,
    /// Explicit port display color, if written.
    pub color: Option<Spanned<[u8; 3]>>,
    /// Explicit port role, if written.
    pub role: Option<Spanned<crate::PortRole>>,
    /// Second endpoint for walking or patch-rotation blocks.
    pub end: Option<Spanned<IVec3>>,
    /// Optional user tag.
    pub tag: Option<Spanned<String>>,
}

/// A parsed block kind, before the two-endpoint kinds are resolved with their
/// movement vectors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockKindAst {
    /// A kind that is fully determined without endpoint geometry.
    Concrete(BlockKind),
    /// A walking block's boundary orientation; movement comes from the endpoints.
    WalkingBoundary(WalkingBoundaryKind),
    /// A patch-rotation block's basis; movement comes from the endpoints.
    PatchRotationBasis(Basis),
}

/// A pipe definition connecting a source to either an explicit destination or a
/// direction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PipeDef {
    /// Whether the pipe changes basis by Hadamard.
    pub hadamard: bool,
    /// Source endpoint reference.
    pub src: Spanned<Ref>,
    /// Destination reference or direction.
    pub dst: Spanned<PipeDst>,
    /// Optional user tag.
    pub tag: Option<Spanned<String>>,
}

/// A pipe destination: an explicit endpoint reference or a step direction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PipeDst {
    /// Explicit endpoint reference.
    Ref(Ref),
    /// Unit step from the source endpoint.
    Dir(crate::Direction),
}

/// `<name> = measure <ref>` or `<name> = measure <ref> -> <dir>`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MeasureDef {
    /// Unresolved node or edge being measured.
    pub target: Spanned<MeasureTargetAst>,
    /// Measurement record name.
    pub name: Spanned<String>,
}

/// Unresolved measure target — either a node reference or an edge
/// (node reference + direction).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MeasureTargetAst {
    /// Node measurement.
    Node(Ref),
    /// Edge measurement identified by an endpoint and direction.
    Edge(Ref, crate::Direction),
}

/// `<name> = <expr>`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LetDef {
    /// Name being defined.
    pub name: Spanned<String>,
    /// Boolean value assigned to the name.
    pub expr: Spanned<Expr>,
}

/// A boolean expression over measurement-outcome variables, with spanned
/// sub-expressions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Expr {
    /// Named Boolean variable.
    Var(String),
    /// Logical negation.
    Not(Box<Spanned<Expr>>),
    /// Binary Boolean operation with ordered operands.
    Binary(BinaryOp, Box<Spanned<Expr>>, Box<Spanned<Expr>>),
}

/// A selective-block reference or a named branch region.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolveTargetDef {
    /// Selective block reference.
    Ref(Ref),
    /// Named structural branch region.
    Branch(String),
}

/// `resolve <ref-or-branch-name> if <expr>`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolveDef {
    /// Selective block or branch being resolved.
    pub target: Spanned<ResolveTargetDef>,
    /// Boolean selector expression.
    pub condition: Spanned<Expr>,
}

/// `feedback <targets> [if <expr>]`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeedbackDef {
    /// Pauli correction targets.
    pub targets: Vec<FeedbackTargetDef>,
    /// Optional Boolean feedback guard.
    pub condition: Option<Spanned<Expr>>,
}

/// A single Pauli correction site within a [`FeedbackDef`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeedbackTargetDef {
    /// Pauli basis of the correction.
    pub pauli: Spanned<PauliBasis>,
    /// Block receiving the correction.
    pub target: Spanned<Ref>,
    /// Optional endpoint wire selecting the target Pauli frame.
    pub direction: Option<Spanned<Direction>>,
}
