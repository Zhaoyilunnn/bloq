//! The BLOG syntax parser.

use chumsky::pratt;
use chumsky::prelude::*;

use crate::action::BinaryOp;
use crate::parser::ParseError;
use crate::parser::ast::{
    ActionStmt, BitOutputDef, BlockDef, BlockKindAst, BranchArmStmt, BranchRegionDef,
    ConnectEndpointAst, ConnectStmt, DataStmt, Expr, FeedbackDef, FeedbackTargetDef, ImportDef,
    InstanceDef, InterfaceStmt, LetDef, MeasureDef, MeasureTargetAst, ModularSourceFile, ModuleDef,
    PipeDef, PipeDst, PortDirectionAst, QuantumPortDef, Ref, ResolveDef, ResolveTargetDef,
    SourceFile, Span, Spanned,
};
use crate::{Basis, BlockKind, WalkingBoundaryKind};
use bloq_utils::PauliBasis;
use glam::IVec3;

type ParserExtra<'src> = extra::Err<Rich<'src, char>>;

fn to_span(s: SimpleSpan) -> Span {
    Span::from_range(s.into_range())
}

// Tokens consume horizontal space only. Statement parsers own newlines, so a
// malformed modifier or expression cannot leak into the following line.
fn hws<'src>() -> impl Parser<'src, &'src str, (), ParserExtra<'src>> + Clone {
    one_of(" \t").repeated().ignored()
}

fn line_comment<'src>() -> impl Parser<'src, &'src str, (), ParserExtra<'src>> + Clone {
    just('#').then(none_of("\r\n").repeated()).ignored()
}

fn newline<'src>() -> impl Parser<'src, &'src str, (), ParserExtra<'src>> + Clone {
    just('\r').or_not().then_ignore(just('\n')).ignored()
}

fn blank_lines<'src>() -> impl Parser<'src, &'src str, (), ParserExtra<'src>> + Clone {
    let line = hws()
        .ignore_then(line_comment().or_not())
        .then_ignore(newline());
    let eof = hws()
        .ignore_then(line_comment().or_not())
        .then_ignore(end());
    line.repeated().then_ignore(eof.or_not()).ignored()
}

fn line_end<'src>() -> impl Parser<'src, &'src str, (), ParserExtra<'src>> + Clone {
    hws()
        .ignore_then(line_comment().or_not())
        .then_ignore(choice((newline(), end())))
        .ignored()
}

fn line<'src, T: 'src>(
    parser: impl Parser<'src, &'src str, T, ParserExtra<'src>> + Clone,
) -> impl Parser<'src, &'src str, T, ParserExtra<'src>> + Clone {
    hws().ignore_then(parser).then_ignore(line_end())
}

/// `p` followed by optional horizontal whitespace.
fn lexeme<'src, T: 'src>(
    p: impl Parser<'src, &'src str, T, ParserExtra<'src>> + Clone,
) -> impl Parser<'src, &'src str, T, ParserExtra<'src>> + Clone {
    p.then_ignore(hws())
}

/// Attach a tight source span to a token parser, then consume trailing
/// whitespace. Capturing the span inside the lexeme is what keeps trailing
/// blank lines and `#`-comments out of the span.
fn spanned<'src, T: 'src>(
    p: impl Parser<'src, &'src str, T, ParserExtra<'src>> + Clone,
) -> impl Parser<'src, &'src str, Spanned<T>, ParserExtra<'src>> + Clone {
    lexeme(p.map_with(|node, e| Spanned::new(node, to_span(e.span()))))
}

/// A statement span: tight start (from the enclosing `e.span()`, which begins at
/// the first token) to the tight end of the statement's last token (`last`),
/// excluding trailing whitespace/comments the final lexeme consumed.
fn stmt_span(outer: SimpleSpan, last: Span) -> Span {
    Span {
        start: to_span(outer).start,
        end: last.end,
    }
}

const KEYWORDS: &[&str] = &[
    "blog", "measure", "resolve", "branch", "false", "true", "feedback", "if",
];

const MODULE_KEYWORDS: &[&str] = &["module", "import"];

fn ci_keyword_raw<'src>(
    kw: &'src str,
) -> impl Parser<'src, &'src str, &'src str, ParserExtra<'src>> + Clone {
    text::ascii::ident().try_map(move |s: &str, span| {
        if s.eq_ignore_ascii_case(kw) {
            Ok(s)
        } else {
            Err(Rich::custom(span, format!("expected keyword `{kw}`")))
        }
    })
}

/// Case-insensitive keyword, consumed as a lexeme.
fn ci_keyword<'src>(
    kw: &'src str,
) -> impl Parser<'src, &'src str, &'src str, ParserExtra<'src>> + Clone {
    lexeme(ci_keyword_raw(kw))
}

// --- Identifier character classes ---

/// First character of an identifier: `[A-Za-z_]`. Requiring a letter or
/// underscore keeps bare numbers, `+X`, and `-`-junk from parsing as names.
fn is_ident_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_'
}

/// Continuation characters of an identifier: `[A-Za-z0-9_/.+-]`. Shared with the
/// Hadamard-flag lookahead so the charset lives in exactly one place.
fn is_ident_continue(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '_' | '/' | '.' | '+' | '-')
}

fn is_simple_ident_continue(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '_' | '+' | '-')
}

fn has_identifier_chars(s: &str, is_continue: impl Fn(char) -> bool) -> bool {
    let mut chars = s.chars();
    matches!(chars.next(), Some(c) if is_ident_start(c)) && chars.all(is_continue)
}

/// Identifier body: `[A-Za-z_][A-Za-z0-9_/.+-]*`. Reserved-keyword checks are
/// layered on by callers that need them.
fn ident_body<'src>() -> impl Parser<'src, &'src str, &'src str, ParserExtra<'src>> + Clone {
    any()
        .filter(|c: &char| is_ident_start(*c))
        .then(any().filter(|c: &char| is_ident_continue(*c)).repeated())
        .to_slice()
}

fn simple_ident_body<'src>() -> impl Parser<'src, &'src str, &'src str, ParserExtra<'src>> + Clone {
    any()
        .filter(|c: &char| is_ident_start(*c))
        .then(
            any()
                .filter(|c: &char| is_simple_ident_continue(*c))
                .repeated(),
        )
        .to_slice()
}

fn simple_ident_raw<'src>() -> impl Parser<'src, &'src str, &'src str, ParserExtra<'src>> + Clone {
    simple_ident_body().try_map(|s: &str, span| {
        if is_valid_simple_identifier(s) {
            Ok(s)
        } else {
            Err(Rich::custom(span, format!("`{s}` is a reserved keyword")))
        }
    })
}

pub(crate) fn is_valid_simple_identifier(s: &str) -> bool {
    has_identifier_chars(s, is_simple_ident_continue)
        && !KEYWORDS
            .iter()
            .chain(MODULE_KEYWORDS)
            .any(|keyword| s.eq_ignore_ascii_case(keyword))
}

pub(crate) fn is_valid_resource_type(s: &str) -> bool {
    has_identifier_chars(s, is_ident_continue)
}

/// Returns whether `s` is a valid BLOG action name (`measure`/`let`).
///
/// Mirrors [`ident_raw`] exactly by reusing the parser's own char classes and
/// keyword list: an [`is_ident_start`] head, an [`is_ident_continue`] tail, and
/// not a reserved keyword. Names are re-emitted verbatim by `to_blog_text`, so
/// enforcing this predicate at the `set_actions` boundary is what guarantees a
/// programmatically set name survives a writer/parser round-trip.
pub(crate) fn is_valid_identifier(s: &str) -> bool {
    has_identifier_chars(s, is_ident_continue)
        && !KEYWORDS.iter().any(|kw| s.eq_ignore_ascii_case(kw))
}

/// Identifier body that rejects reserved keywords (case-insensitive), without
/// trailing-whitespace consumption.
fn ident_raw<'src>() -> impl Parser<'src, &'src str, &'src str, ParserExtra<'src>> + Clone {
    ident_body().try_map(|s: &str, span| {
        if KEYWORDS.iter().any(|kw| s.eq_ignore_ascii_case(kw)) {
            Err(Rich::custom(span, format!("`{s}` is a reserved keyword")))
        } else {
            Ok(s)
        }
    })
}

/// Signed integer literal.
fn integer<'src>() -> impl Parser<'src, &'src str, i32, ParserExtra<'src>> + Clone {
    lexeme(
        just('-')
            .or_not()
            .then(text::digits(10))
            .to_slice()
            .try_map(|s: &str, span| {
                s.parse::<i32>()
                    .map_err(|e| Rich::custom(span, e.to_string()))
            }),
    )
}

/// Unsigned integer literal without trailing-whitespace consumption.
fn unsigned_raw<'src>() -> impl Parser<'src, &'src str, u32, ParserExtra<'src>> + Clone {
    text::int(10).try_map(|s: &str, span| {
        s.parse::<u32>()
            .map_err(|e| Rich::custom(span, e.to_string()))
    })
}

/// `[x, y, z]` position literal.
fn position<'src>() -> impl Parser<'src, &'src str, Spanned<IVec3>, ParserExtra<'src>> + Clone {
    let coords = integer()
        .separated_by(lexeme(just(',')))
        .exactly(3)
        .collect::<Vec<_>>();
    // The opening `[` starts the span; the closing `]` (kept out of a lexeme)
    // ends it, so trailing whitespace lands outside the literal's span.
    spanned(
        coords
            .delimited_by(just('[').then_ignore(hws()), just(']'))
            .map(|coords| IVec3::new(coords[0], coords[1], coords[2])),
    )
}

/// Cube height expression `[<num>] d [/ <den>] [(+|-) <n>]`, with horizontal space
/// tolerated around `/` and the offset sign so `2d`, `3d+2` and `d/2 + 1` all
/// read. Shared by the `height=` modifier and [`CubeHeight::from_str`], so the
/// surface syntax has exactly one definition.
fn cube_height_expr<'src>()
-> impl Parser<'src, &'src str, crate::CubeHeight, ParserExtra<'src>> + Clone {
    let denominator = hws()
        .ignore_then(just('/'))
        .ignore_then(hws())
        .ignore_then(unsigned_raw());
    let offset = hws()
        .ignore_then(one_of("+-"))
        .then_ignore(hws())
        .then(unsigned_raw());

    unsigned_raw()
        .or_not()
        .then_ignore(hws())
        .then_ignore(one_of("dD"))
        .then(denominator.or_not())
        .then(offset.or_not())
        .try_map(|((numerator, denominator), offset), span| {
            let offset = match offset {
                None => 0,
                Some((sign, magnitude)) => {
                    let magnitude = i64::from(magnitude);
                    i32::try_from(if sign == '-' { -magnitude } else { magnitude })
                        .map_err(|_| Rich::custom(span, "cube height offset is out of range"))?
                }
            };
            crate::CubeHeight::new(numerator.unwrap_or(1), denominator.unwrap_or(1), offset)
                .map_err(|e| Rich::custom(span, e.to_string()))
        })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BlockModifier {
    Height(crate::CubeHeight),
    Color([u8; 3]),
    Role(crate::PortRole),
}

/// One typed `attribute=value` block modifier.
fn block_modifier<'src>()
-> impl Parser<'src, &'src str, Spanned<BlockModifier>, ParserExtra<'src>> + Clone {
    let height = ci_keyword("height")
        .ignore_then(lexeme(just('=')))
        .ignore_then(cube_height_expr())
        .map(BlockModifier::Height);
    let color = ci_keyword("color")
        .ignore_then(lexeme(just('=')))
        .ignore_then(text::digits(16).to_slice().try_map(|text: &str, span| {
            if text.len() != 6 {
                return Err(Rich::custom(
                    span,
                    "color must contain exactly six hex digits",
                ));
            }
            let [_, r, g, b] = u32::from_str_radix(text, 16)
                .expect("hex digit parser validated value")
                .to_be_bytes();
            Ok(BlockModifier::Color([r, g, b]))
        }));
    let role = ci_keyword("role")
        .ignore_then(lexeme(just('=')))
        .ignore_then(choice((
            ci_keyword("auto").to(crate::PortRole::Auto),
            ci_keyword("input").to(crate::PortRole::Input),
            ci_keyword("output").to(crate::PortRole::Output),
            ci_keyword("multiplex").to(crate::PortRole::Multiplex),
        )))
        .map(BlockModifier::Role);

    spanned(choice((height, color, role)))
}

/// Parses a bare height expression; the engine behind [`CubeHeight::from_str`].
pub(crate) fn cube_height_from_str(text: &str) -> Result<crate::CubeHeight, crate::BlockError> {
    hws()
        .ignore_then(cube_height_expr())
        .then_ignore(hws())
        .then_ignore(end())
        .parse(text)
        .into_result()
        .map_err(|_| crate::BlockError::InvalidCubeHeight(text.to_string()))
}

/// Direction literal: `+X`, `-Z`, etc.
fn direction<'src>()
-> impl Parser<'src, &'src str, Spanned<crate::Direction>, ParserExtra<'src>> + Clone {
    spanned(
        one_of("+-")
            .then(one_of("XYZxyz"))
            .to_slice()
            .try_map(|s: &str, span| {
                s.to_uppercase()
                    .parse::<crate::Direction>()
                    .map_err(|e| Rich::custom(span, e.to_string()))
            }),
    )
}

/// Reference — position literal or block ID.
fn ref_parser<'src>() -> impl Parser<'src, &'src str, Spanned<Ref>, ParserExtra<'src>> + Clone {
    choice((
        position().map(|sp| Spanned::new(Ref::Pos(sp.node), sp.span)),
        spanned(unsigned_raw().map(Ref::Id)),
    ))
}

fn selective_block_kind_token(token: &str) -> Option<(crate::SelectiveKind, bool)> {
    match token.to_ascii_uppercase().as_str() {
        "XY" => Some((crate::SelectiveKind::XY, false)),
        "YX" => Some((crate::SelectiveKind::XY, true)),
        "XZ" => Some((crate::SelectiveKind::XZ, false)),
        "ZX" => Some((crate::SelectiveKind::XZ, true)),
        "YZ" => Some((crate::SelectiveKind::YZ, false)),
        "ZY" => Some((crate::SelectiveKind::YZ, true)),
        _ => None,
    }
}

/// Concrete kind, `walk <boundary>`, or `rotate <basis>`.
fn block_kind<'src>()
-> impl Parser<'src, &'src str, (Spanned<BlockKindAst>, bool), ParserExtra<'src>> + Clone {
    let token = any()
        .filter(|c: &char| c.is_ascii_alphanumeric() || matches!(c, '_' | '+' | '-'))
        .repeated()
        .at_least(1)
        .to_slice();

    let walking = ci_keyword("walk").ignore_then(lexeme(token.try_map(|s: &str, span| {
        let kind = s
            .to_ascii_uppercase()
            .parse::<WalkingBoundaryKind>()
            .map_err(|error| Rich::custom(span, error.to_string()))?;
        Ok((
            Spanned::new(BlockKindAst::WalkingBoundary(kind), to_span(span)),
            false,
        ))
    })));
    let rotation = ci_keyword("rotate").ignore_then(lexeme(token.try_map(|s: &str, span| {
        let basis = match s.to_ascii_uppercase().as_str() {
            "X" => Basis::X,
            "Z" => Basis::Z,
            _ => return Err(Rich::custom(span, "rotation basis must be X or Z")),
        };
        Ok((
            Spanned::new(BlockKindAst::PatchRotationBasis(basis), to_span(span)),
            false,
        ))
    })));
    let concrete = lexeme(token.try_map(|s: &str, span| {
        if let Some((kind, reversed)) = selective_block_kind_token(s) {
            return Ok((
                Spanned::new(
                    BlockKindAst::Concrete(BlockKind::Selective(kind)),
                    to_span(span),
                ),
                reversed,
            ));
        }
        let kind = s
            .parse::<BlockKind>()
            .map_err(|e| Rich::custom(span, e.to_string()))?;
        Ok((
            Spanned::new(BlockKindAst::Concrete(kind), to_span(span)),
            false,
        ))
    }));
    choice((walking, rotation, concrete))
}

/// `<label>` using the [`crate::is_valid_tag`] character grammar.
fn tag<'src>() -> impl Parser<'src, &'src str, Spanned<String>, ParserExtra<'src>> + Clone {
    let body = any()
        .filter(|c: &char| crate::block::is_tag_char(*c))
        .repeated()
        .at_least(1)
        .to_slice();
    spanned(body.map(str::to_owned).delimited_by(just('<'), just('>')))
}

/// Pratt expression parser: `|` < `^` < `&` < prefix `!`.
fn expr_parser<'src>() -> impl Parser<'src, &'src str, Spanned<Expr>, ParserExtra<'src>> + Clone {
    let var = spanned(ident_raw().map(|s| Expr::Var(s.to_string())));

    recursive(|expr| {
        let atom = choice((var, expr.delimited_by(lexeme(just('(')), lexeme(just(')')))));

        // Operator spans are built from the operand spans (tight), not from
        // `e.span()`, which would include the operand's trailing whitespace.
        atom.pratt((
            pratt::prefix(3, lexeme(just('!')), |_op, rhs: Spanned<Expr>, e| {
                let span = Span {
                    start: to_span(e.span()).start,
                    end: rhs.span.end,
                };
                Spanned::new(Expr::Not(Box::new(rhs)), span)
            }),
            pratt::infix(
                pratt::left(2),
                lexeme(just('&')),
                |lhs: Spanned<Expr>, _op, rhs: Spanned<Expr>, _e| {
                    let span = Span {
                        start: lhs.span.start,
                        end: rhs.span.end,
                    };
                    Spanned::new(
                        Expr::Binary(BinaryOp::And, Box::new(lhs), Box::new(rhs)),
                        span,
                    )
                },
            ),
            pratt::infix(
                pratt::left(1),
                lexeme(just('^')),
                |lhs: Spanned<Expr>, _op, rhs: Spanned<Expr>, _e| {
                    let span = Span {
                        start: lhs.span.start,
                        end: rhs.span.end,
                    };
                    Spanned::new(
                        Expr::Binary(BinaryOp::Xor, Box::new(lhs), Box::new(rhs)),
                        span,
                    )
                },
            ),
            pratt::infix(
                pratt::left(0),
                lexeme(just('|')),
                |lhs: Spanned<Expr>, _op, rhs: Spanned<Expr>, _e| {
                    let span = Span {
                        start: lhs.span.start,
                        end: rhs.span.end,
                    };
                    Spanned::new(
                        Expr::Binary(BinaryOp::Or, Box::new(lhs), Box::new(rhs)),
                        span,
                    )
                },
            ),
        ))
    })
}

/// `BLOG <major>.<minor>` — version header at the top of the file.
fn version_header<'src>()
-> impl Parser<'src, &'src str, Spanned<(u32, u32)>, ParserExtra<'src>> + Clone {
    ci_keyword("blog")
        .ignore_then(unsigned_raw().then_ignore(just('.')).then(unsigned_raw()))
        .map_with(|(major, minor), e| Spanned::new((major, minor), to_span(e.span())))
}

// --- Graph statements ---

/// `<id>: <kind> <position> [-> <end>] [attribute=value ...] [<tag>]`.
fn block_def<'src>() -> impl Parser<'src, &'src str, Spanned<DataStmt>, ParserExtra<'src>> + Clone {
    spanned(unsigned_raw())
        .then_ignore(lexeme(just(':')))
        .then(block_kind())
        .then(position())
        .then(lexeme(just("->")).ignore_then(position()).or_not())
        .then(block_modifier().repeated().collect::<Vec<_>>())
        .then(tag().or_not())
        .try_map_with(
            |(((((id, (kind, selective_order_reversed)), pos), end), modifiers), tag), e| {
                let modifier_span = modifiers.last().map(|modifier| modifier.span);
                let mut height = None;
                let mut color = None;
                let mut role = None;
                for modifier in modifiers {
                    let (slot_used, name) = match modifier.node {
                        BlockModifier::Height(value) => {
                            let used = height.replace(Spanned::new(value, modifier.span)).is_some();
                            (used, "height")
                        }
                        BlockModifier::Color(value) => {
                            let used = color.replace(Spanned::new(value, modifier.span)).is_some();
                            (used, "color")
                        }
                        BlockModifier::Role(value) => {
                            let used = role.replace(Spanned::new(value, modifier.span)).is_some();
                            (used, "role")
                        }
                    };
                    if slot_used {
                        return Err(Rich::custom(
                            SimpleSpan::new(
                                (),
                                modifier.span.start as usize..modifier.span.end as usize,
                            ),
                            format!("duplicate {name} block modifier"),
                        ));
                    }
                }
                // The statement ends at whichever trailing element is present.
                let last = tag
                    .as_ref()
                    .map(|t| t.span)
                    .or(modifier_span)
                    .or_else(|| end.as_ref().map(|x| x.span))
                    .unwrap_or(pos.span);
                Ok(Spanned::new(
                    DataStmt::Block(BlockDef {
                        id,
                        kind,
                        selective_order_reversed,
                        pos,
                        height,
                        color,
                        role,
                        end,
                        tag,
                    }),
                    stmt_span(e.span(), last),
                ))
            },
        )
}

/// Ordinary (`->`) or Hadamard (`-H>`) edge.
fn arrow<'src>() -> impl Parser<'src, &'src str, bool, ParserExtra<'src>> + Clone {
    lexeme(choice((
        just("-H>").to(true),
        just("-h>").to(true),
        just("->").to(false),
    )))
}

/// Pipe destination: position, direction, or block ID.
fn pipe_dst<'src>() -> impl Parser<'src, &'src str, Spanned<PipeDst>, ParserExtra<'src>> + Clone {
    choice((
        position().map(|sp| Spanned::new(PipeDst::Ref(Ref::Pos(sp.node)), sp.span)),
        direction().map(|sp| Spanned::new(PipeDst::Dir(sp.node), sp.span)),
        spanned(unsigned_raw().map(|id| PipeDst::Ref(Ref::Id(id)))),
    ))
}

/// `<src> (->|-H>) <dst_or_dir> [<tag>]`
fn pipe_def<'src>() -> impl Parser<'src, &'src str, Spanned<DataStmt>, ParserExtra<'src>> + Clone {
    ref_parser()
        .then(arrow())
        .then(pipe_dst())
        .then(tag().or_not())
        .map_with(|(((src, hadamard), dst), tag), e| {
            let last = tag.as_ref().map(|t| t.span).unwrap_or(dst.span);
            Spanned::new(
                DataStmt::Pipe(PipeDef {
                    hadamard,
                    src,
                    dst,
                    tag,
                }),
                stmt_span(e.span(), last),
            )
        })
}

fn data_stmt<'src>() -> impl Parser<'src, &'src str, Spanned<DataStmt>, ParserExtra<'src>> + Clone {
    choice((branch_region_def(), block_def(), pipe_def()))
}

fn branch_arm_stmt<'src>()
-> impl Parser<'src, &'src str, Spanned<BranchArmStmt>, ParserExtra<'src>> + Clone {
    choice((
        block_def().map(|stmt| {
            let DataStmt::Block(def) = stmt.node else {
                unreachable!("block_def returns a block")
            };
            Spanned::new(BranchArmStmt::Block(def), stmt.span)
        }),
        pipe_def().map(|stmt| {
            let DataStmt::Pipe(def) = stmt.node else {
                unreachable!("pipe_def returns a pipe")
            };
            Spanned::new(BranchArmStmt::Pipe(def), stmt.span)
        }),
    ))
}

/// `branch <name> { false { <arm> } true { <arm> } }`
fn branch_region_def<'src>()
-> impl Parser<'src, &'src str, Spanned<DataStmt>, ParserExtra<'src>> + Clone {
    let arm = line(branch_arm_stmt())
        .then_ignore(blank_lines())
        .repeated()
        .collect::<Vec<_>>();
    let false_arm = hws()
        .ignore_then(ci_keyword("false"))
        .then_ignore(just('{'))
        .then_ignore(line_end())
        .then_ignore(blank_lines())
        .ignore_then(arm.clone())
        .then_ignore(hws().ignore_then(just('}')))
        .then_ignore(line_end())
        .then_ignore(blank_lines());
    let true_arm = hws()
        .ignore_then(ci_keyword("true"))
        .then_ignore(just('{'))
        .then_ignore(line_end())
        .then_ignore(blank_lines())
        .ignore_then(arm)
        .then_ignore(hws().ignore_then(just('}')))
        .then_ignore(line_end())
        .then_ignore(blank_lines());
    ci_keyword("branch")
        .ignore_then(spanned(ident_raw().map(str::to_owned)))
        .then_ignore(just('{'))
        .then_ignore(line_end())
        .then_ignore(blank_lines())
        .then(false_arm)
        .then(true_arm)
        .then_ignore(hws().ignore_then(just('}')))
        .map_with(|((name, on_false), on_true), e| {
            Spanned::new(
                DataStmt::Branch(BranchRegionDef {
                    name,
                    on_false,
                    on_true,
                }),
                to_span(e.span()),
            )
        })
}

// --- Action statements ---

fn measure_target<'src>()
-> impl Parser<'src, &'src str, Spanned<MeasureTargetAst>, ParserExtra<'src>> + Clone {
    let pipe_target = ref_parser()
        .then_ignore(lexeme(just("->")))
        .then(direction())
        .map(|(r, dir)| {
            let span = Span {
                start: r.span.start,
                end: dir.span.end,
            };
            Spanned::new(MeasureTargetAst::Edge(r.node, dir.node), span)
        });

    let block_target = ref_parser().map(|r| Spanned::new(MeasureTargetAst::Node(r.node), r.span));

    choice((pipe_target, block_target))
}

enum BindingRhs {
    Measure(Spanned<MeasureTargetAst>),
    Expr(Spanned<Expr>),
}

/// `<name> = measure <target>` or `<name> = <expr>`.
fn binding_def<'src>()
-> impl Parser<'src, &'src str, Spanned<ActionStmt>, ParserExtra<'src>> + Clone {
    let rhs = choice((
        ci_keyword("measure")
            .ignore_then(measure_target())
            .map(BindingRhs::Measure),
        expr_parser().map(BindingRhs::Expr),
    ));
    spanned(ident_raw().map(str::to_owned))
        .then_ignore(lexeme(just('=')))
        .then(rhs)
        .map_with(|(name, rhs), e| {
            let (node, last) = match rhs {
                BindingRhs::Measure(target) => {
                    let last = target.span;
                    (ActionStmt::Measure(MeasureDef { target, name }), last)
                }
                BindingRhs::Expr(expr) => {
                    let last = expr.span;
                    (ActionStmt::Let(LetDef { name, expr }), last)
                }
            };
            Spanned::new(node, stmt_span(e.span(), last))
        })
}

/// `discard if <expr>`.
fn discard_def<'src>()
-> impl Parser<'src, &'src str, Spanned<ActionStmt>, ParserExtra<'src>> + Clone {
    ci_keyword("discard")
        .ignore_then(ci_keyword("if"))
        .ignore_then(expr_parser())
        .map_with(|expr, e| {
            let span = stmt_span(e.span(), expr.span);
            Spanned::new(ActionStmt::DiscardIf(expr), span)
        })
}

/// `resolve <ref-or-branch-name> if <expr>`.
fn resolve_def<'src>()
-> impl Parser<'src, &'src str, Spanned<ActionStmt>, ParserExtra<'src>> + Clone {
    let target = choice((
        ref_parser().map(|target| Spanned::new(ResolveTargetDef::Ref(target.node), target.span)),
        spanned(ident_raw().map(|name| ResolveTargetDef::Branch(name.to_owned()))),
    ));
    ci_keyword("resolve")
        .ignore_then(target)
        .then_ignore(ci_keyword("if"))
        .then(expr_parser())
        .map_with(|(target, condition), e| {
            let span = stmt_span(e.span(), condition.span);
            Spanned::new(ActionStmt::Resolve(ResolveDef { target, condition }), span)
        })
}

/// Pauli basis: case-insensitive `X`, `Y`, `Z`.
fn pauli<'src>() -> impl Parser<'src, &'src str, Spanned<PauliBasis>, ParserExtra<'src>> + Clone {
    spanned(one_of("XYZxyz").try_map(|c: char, span| {
        c.to_uppercase()
            .to_string()
            .parse::<PauliBasis>()
            .map_err(|e| Rich::custom(span, e.to_string()))
    }))
}

/// `feedback <pauli> <ref> [-> <dir>] [, ...] [if <expr>]`.
fn feedback_def<'src>()
-> impl Parser<'src, &'src str, Spanned<ActionStmt>, ParserExtra<'src>> + Clone {
    let feedback_target = pauli()
        .then(ref_parser())
        .then(lexeme(just("->")).ignore_then(direction()).or_not())
        .map(|((pauli, target), direction)| FeedbackTargetDef {
            pauli,
            target,
            direction,
        });

    ci_keyword("feedback")
        .ignore_then(
            feedback_target
                .separated_by(lexeme(just(',')))
                .at_least(1)
                .collect::<Vec<_>>(),
        )
        .then(ci_keyword("if").ignore_then(expr_parser()).or_not())
        .map_with(|(targets, condition), e| {
            let last = condition.as_ref().map(|c| c.span).unwrap_or_else(|| {
                let last = targets.last().expect("feedback parses at least one target");
                last.direction
                    .as_ref()
                    .map_or(last.target.span, |direction| direction.span)
            });
            Spanned::new(
                ActionStmt::Feedback(FeedbackDef { targets, condition }),
                stmt_span(e.span(), last),
            )
        })
}

fn action_stmt<'src>()
-> impl Parser<'src, &'src str, Spanned<ActionStmt>, ParserExtra<'src>> + Clone {
    choice((binding_def(), discard_def(), resolve_def(), feedback_def()))
}

// --- Module statements ---

fn port_direction<'src>()
-> impl Parser<'src, &'src str, Spanned<PortDirectionAst>, ParserExtra<'src>> + Clone {
    spanned(choice((
        ci_keyword_raw("in").to(PortDirectionAst::Input),
        ci_keyword_raw("out").to(PortDirectionAst::Output),
    )))
}

enum InterfaceTail {
    Quantum(Spanned<String>, Spanned<u32>),
    BitOutput(Spanned<Expr>),
}

fn interface_stmt<'src>()
-> impl Parser<'src, &'src str, Spanned<InterfaceStmt>, ParserExtra<'src>> + Clone {
    let quantum = lexeme(just(':'))
        .ignore_then(spanned(ident_body().map(str::to_owned)))
        .then_ignore(lexeme(just('=')))
        .then(spanned(unsigned_raw()))
        .map(|(resource_type, block_id)| InterfaceTail::Quantum(resource_type, block_id));
    let bit_output = lexeme(just('='))
        .ignore_then(expr_parser())
        .map(InterfaceTail::BitOutput);

    port_direction()
        .then(spanned(simple_ident_raw().map(str::to_owned)))
        .then(choice((quantum, bit_output)).or_not())
        .try_map_with(|((direction, name), tail), e| {
            let (node, last) = match (direction.node, tail) {
                (_, Some(InterfaceTail::Quantum(resource_type, block_id))) => {
                    let last = block_id.span;
                    (
                        InterfaceStmt::Quantum(QuantumPortDef {
                            direction,
                            name,
                            block_id,
                            resource_type,
                        }),
                        last,
                    )
                }
                (PortDirectionAst::Input, None) => {
                    let last = name.span;
                    (InterfaceStmt::BitInput(name), last)
                }
                (PortDirectionAst::Output, Some(InterfaceTail::BitOutput(expr))) => {
                    let last = expr.span;
                    (InterfaceStmt::BitOutput(BitOutputDef { name, expr }), last)
                }
                (PortDirectionAst::Input, Some(InterfaceTail::BitOutput(_))) => {
                    return Err(Rich::custom(
                        e.span(),
                        "classical input cannot have a value",
                    ));
                }
                (PortDirectionAst::Output, None) => {
                    return Err(Rich::custom(e.span(), "classical output requires a value"));
                }
            };
            Ok(Spanned::new(node, stmt_span(e.span(), last)))
        })
}

fn instance_def<'src>()
-> impl Parser<'src, &'src str, Spanned<InstanceDef>, ParserExtra<'src>> + Clone {
    let rotation = spanned(
        ci_keyword("rotate")
            .ignore_then(one_of("XYZxyz"))
            .then_ignore(hws())
            .then(integer())
            .map(|(axis, degrees)| {
                let axis = match axis.to_ascii_uppercase() {
                    'X' => crate::UDirection::X,
                    'Y' => crate::UDirection::Y,
                    'Z' => crate::UDirection::Z,
                    _ => unreachable!("axis parser accepts only X, Y, or Z"),
                };
                (axis, degrees)
            }),
    );
    spanned(simple_ident_raw().map(str::to_owned))
        .then_ignore(lexeme(just(':')))
        .then(spanned(simple_ident_raw().map(str::to_owned)))
        .then_ignore(lexeme(just('@')))
        .then(position())
        .then(rotation.or_not())
        .map_with(|(((name, definition), translation), rotation), e| {
            let span = stmt_span(
                e.span(),
                rotation
                    .as_ref()
                    .map_or(translation.span, |rotation| rotation.span),
            );
            Spanned::new(
                InstanceDef {
                    name,
                    definition,
                    translation,
                    rotation,
                },
                span,
            )
        })
}

fn member<'src>() -> impl Parser<'src, &'src str, Spanned<String>, ParserExtra<'src>> + Clone {
    spanned(simple_ident_raw())
        .then_ignore(lexeme(just('.')))
        .then(spanned(simple_ident_raw()))
        .map(|(instance, port)| {
            Spanned::new(
                format!("{}.{}", instance.node, port.node),
                Span {
                    start: instance.span.start,
                    end: port.span.end,
                },
            )
        })
}

fn connect_stmt<'src>()
-> impl Parser<'src, &'src str, Spanned<ConnectStmt>, ParserExtra<'src>> + Clone {
    let block = || spanned(unsigned_raw());
    let bit_source = choice((member(), spanned(simple_ident_raw().map(str::to_owned))));
    let bit_bind = bit_source
        .then_ignore(lexeme(just("=>")))
        .then(member())
        .map_with(|(source, target), e| {
            let span = stmt_span(e.span(), target.span);
            Spanned::new(
                ConnectStmt::Bind {
                    hadamard: false,
                    source: Spanned::new(ConnectEndpointAst::Name(source.node), source.span),
                    target: Spanned::new(ConnectEndpointAst::Name(target.node), target.span),
                },
                span,
            )
        });
    let direct_pipe =
        member()
            .then(arrow())
            .then(member())
            .map_with(|((output, hadamard), input), e| {
                let span = stmt_span(e.span(), input.span);
                Spanned::new(
                    ConnectStmt::Pipe {
                        hadamard,
                        output,
                        input,
                    },
                    span,
                )
            });
    let input_bind =
        block()
            .then(arrow())
            .then(member())
            .map_with(|((block, hadamard), target), e| {
                let span = stmt_span(e.span(), target.span);
                Spanned::new(
                    ConnectStmt::Bind {
                        hadamard,
                        source: Spanned::new(ConnectEndpointAst::Block(block.node), block.span),
                        target: Spanned::new(ConnectEndpointAst::Name(target.node), target.span),
                    },
                    span,
                )
            });
    let output_bind =
        member()
            .then(arrow())
            .then(block())
            .map_with(|((source, hadamard), block), e| {
                let span = stmt_span(e.span(), block.span);
                Spanned::new(
                    ConnectStmt::Bind {
                        hadamard,
                        source: Spanned::new(ConnectEndpointAst::Name(source.node), source.span),
                        target: Spanned::new(ConnectEndpointAst::Block(block.node), block.span),
                    },
                    span,
                )
            });
    choice((bit_bind, direct_pipe, input_bind, output_bind))
}

fn quoted_path<'src>() -> impl Parser<'src, &'src str, Spanned<String>, ParserExtra<'src>> + Clone {
    spanned(
        none_of("\"\r\n")
            .repeated()
            .to_slice()
            .delimited_by(just('"'), just('"'))
            .map(str::to_owned),
    )
}

fn import_def<'src>() -> impl Parser<'src, &'src str, Spanned<ImportDef>, ParserExtra<'src>> + Clone
{
    ci_keyword("import")
        .ignore_then(quoted_path())
        .then_ignore(ci_keyword("as"))
        .then(spanned(simple_ident_raw().map(str::to_owned)))
        .map_with(|(path, alias), e| {
            let span = stmt_span(e.span(), alias.span);
            Spanned::new(ImportDef { path, alias }, span)
        })
}

enum ModuleStmt {
    Interface(Spanned<InterfaceStmt>),
    Instance(Spanned<InstanceDef>),
    Data(Spanned<DataStmt>),
    Action(Spanned<ActionStmt>),
    Connect(Spanned<ConnectStmt>),
}

fn module_stmt<'src>() -> impl Parser<'src, &'src str, ModuleStmt, ParserExtra<'src>> + Clone {
    choice((
        data_stmt().map(ModuleStmt::Data),
        action_stmt().map(ModuleStmt::Action),
        interface_stmt().map(ModuleStmt::Interface),
        instance_def().map(ModuleStmt::Instance),
        connect_stmt().map(ModuleStmt::Connect),
    ))
}

fn module_open<'src>() -> impl Parser<'src, &'src str, Spanned<String>, ParserExtra<'src>> + Clone {
    ci_keyword("module")
        .ignore_then(spanned(simple_ident_raw().map(str::to_owned)))
        .then_ignore(just('{'))
        .then_ignore(line_end())
}

fn module_def<'src>() -> impl Parser<'src, &'src str, Spanned<ModuleDef>, ParserExtra<'src>> + Clone
{
    module_open()
        .then_ignore(blank_lines())
        .then(
            line(module_stmt())
                .then_ignore(blank_lines())
                .repeated()
                .collect::<Vec<_>>(),
        )
        .then_ignore(hws().ignore_then(just('}')))
        .map_with(|(name, statements), e| {
            let mut module = ModuleDef {
                name,
                interface_stmts: Vec::new(),
                instance_stmts: Vec::new(),
                data_stmts: Vec::new(),
                action_stmts: Vec::new(),
                connect_stmts: Vec::new(),
            };
            for statement in statements {
                match statement {
                    ModuleStmt::Interface(stmt) => module.interface_stmts.push(stmt),
                    ModuleStmt::Instance(stmt) => module.instance_stmts.push(stmt),
                    ModuleStmt::Data(stmt) => module.data_stmts.push(stmt),
                    ModuleStmt::Action(stmt) => module.action_stmts.push(stmt),
                    ModuleStmt::Connect(stmt) => module.connect_stmts.push(stmt),
                }
            }
            Spanned::new(module, to_span(e.span()))
        })
}

enum FileStmt {
    Data(Spanned<DataStmt>),
    Action(Spanned<ActionStmt>),
}

fn file_stmt<'src>() -> impl Parser<'src, &'src str, FileStmt, ParserExtra<'src>> + Clone {
    choice((
        data_stmt().map(FileStmt::Data),
        action_stmt().map(FileStmt::Action),
    ))
}

pub(super) fn parse(input: &str) -> Result<SourceFile, ParseError> {
    let parser = blank_lines()
        .ignore_then(line(version_header()))
        .then_ignore(blank_lines())
        .then(
            line(file_stmt())
                .then_ignore(blank_lines())
                .repeated()
                .collect::<Vec<_>>(),
        )
        .then_ignore(end());

    match parser.parse(input).into_result() {
        Ok((version, statements)) => {
            let mut source = SourceFile {
                version,
                data_stmts: Vec::new(),
                action_stmts: Vec::new(),
            };
            for statement in statements {
                match statement {
                    FileStmt::Data(stmt) => source.data_stmts.push(stmt),
                    FileStmt::Action(stmt) => source.action_stmts.push(stmt),
                }
            }
            Ok(source)
        }
        Err(errors) => Err(syntax_error(&errors)),
    }
}

pub(super) fn parse_program(input: &str) -> Result<ModularSourceFile, ParseError> {
    let parser = blank_lines()
        .ignore_then(line(version_header()))
        .then_ignore(blank_lines())
        .then(
            line(import_def())
                .then_ignore(blank_lines())
                .repeated()
                .collect::<Vec<_>>(),
        )
        .then(
            line(module_def())
                .then_ignore(blank_lines())
                .repeated()
                .collect::<Vec<_>>(),
        )
        .then_ignore(blank_lines())
        .then_ignore(end());

    match parser.parse(input).into_result() {
        Ok(((version, imports), modules)) => Ok(ModularSourceFile {
            version,
            imports,
            modules,
        }),
        Err(errors) => Err(syntax_error(&errors)),
    }
}

pub(super) fn parse_actions_only(input: &str) -> Result<Vec<Spanned<ActionStmt>>, ParseError> {
    let parser = blank_lines()
        .ignore_then(
            line(action_stmt())
                .then_ignore(blank_lines())
                .repeated()
                .collect::<Vec<_>>(),
        )
        .then_ignore(end());

    let result = parser.parse(input);

    match result.into_result() {
        Ok(stmts) => Ok(stmts),
        Err(errors) => Err(syntax_error(&errors)),
    }
}

fn syntax_error(errors: &[Rich<'_, char>]) -> ParseError {
    let err = errors.first().expect("chumsky yields ≥1 error on failure");
    ParseError::Syntax {
        message: err.to_string(),
        span: to_span(*err.span()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_ok(input: &str) -> SourceFile {
        parse(input).expect("input should parse")
    }

    fn slice(input: &str, span: Span) -> &str {
        &input[span.start as usize..span.end as usize]
    }

    // ---- Span hygiene --------------------------------------------------------

    #[test]
    fn version_span_covers_only_the_header_tokens() {
        let input = "BLOG 1.0\n\n0: XZZ [0,0,0]\n";
        let file = parse_ok(input);
        // "BLOG 1.0" is bytes 0..8; the span must not swallow the blank lines.
        assert_eq!((file.version.span.start, file.version.span.end), (0, 8));
        assert_eq!(slice(input, file.version.span), "BLOG 1.0");
    }

    #[test]
    fn unsupported_version_span_stops_at_the_header() {
        // `parse` does not reject 2.0 (lowering does), but the span it hands to
        // the diagnostic must underline only `BLOG 2.0`, not the blank lines.
        let input = "BLOG 2.0\n\n";
        let file = parse_ok(input);
        assert_eq!(slice(input, file.version.span), "BLOG 2.0");
    }

    #[test]
    fn block_statement_span_ends_at_last_token_not_newline() {
        let input = "BLOG 1.0\n\n0: XZZ [0,0,0]\n";
        let file = parse_ok(input);
        assert_eq!(slice(input, file.data_stmts[0].span), "0: XZZ [0,0,0]");
    }

    #[test]
    fn block_statement_span_includes_trailing_tag() {
        let input = "BLOG 1.0\n\n0: ZXZ [0,0,0] <wide>\n";
        let file = parse_ok(input);
        assert_eq!(
            slice(input, file.data_stmts[0].span),
            "0: ZXZ [0,0,0] <wide>"
        );
    }

    #[test]
    fn action_statement_span_excludes_trailing_comment_and_newline() {
        let input = "x = a  # note\ny = b";
        let stmts = parse_actions_only(input).expect("actions should parse");
        assert_eq!(slice(input, stmts[0].span), "x = a");
        assert_eq!(slice(input, stmts[1].span), "y = b");
    }

    #[test]
    fn final_comment_needs_no_newline() {
        parse("BLOG 1.0\n0: XZZ [0,0,0]\n# done").unwrap();
        parse("BLOG 1.0\n0: XZZ [0,0,0]\n  ").unwrap();
    }

    #[test]
    fn removed_sections_are_rejected() {
        parse("BLOG 1.0\n.data\n").unwrap_err();
        parse("BLOG 1.0\n.action\n").unwrap_err();
    }

    #[test]
    fn statements_cannot_continue_across_lines() {
        parse("BLOG 1.0\n0: XZZ [0,0,0] height=\nd\n").unwrap_err();
    }

    // ---- Identifier tightening ----------------------------------------------

    #[test]
    fn numeric_binding_names_are_rejected() {
        for input in ["0 = a", "1 = measure 0"] {
            let err = parse_actions_only(input).expect_err("numeric name should be rejected");
            assert!(matches!(err, ParseError::Syntax { .. }), "{input}");
        }
    }

    // ---- Cube height ---------------------------------------------------------

    fn parsed_height(modifier: &str) -> Result<Option<crate::CubeHeight>, ParseError> {
        let input = format!("BLOG 1.0\n\n0: XZZ [0,0,0] {modifier}\n");
        let file = parse(&input)?;
        let DataStmt::Block(def) = &file.data_stmts[0].node else {
            panic!("expected block");
        };
        Ok(def.height.as_ref().map(|h| h.node))
    }

    fn parsed_port(
        modifier: &str,
    ) -> Result<(Option<[u8; 3]>, Option<crate::PortRole>), ParseError> {
        let input = format!("BLOG 1.0\n\n0: Port [0,0,0] {modifier}\n");
        let file = parse(&input)?;
        let DataStmt::Block(def) = &file.data_stmts[0].node else {
            panic!("expected block");
        };
        Ok((
            def.color.as_ref().map(|color| color.node),
            def.role.as_ref().map(|role| role.node),
        ))
    }

    /// The expression grammar itself is covered in `height.rs`, which now shares
    /// it; what is left here is the modifier around it.
    #[test]
    fn block_modifiers_are_optional_and_case_insensitive() {
        assert_eq!(parsed_height("").expect("parses"), None);
        assert_eq!(
            parsed_height("HEIGHT = 2D").expect("parses"),
            Some("2d".parse().expect("valid height"))
        );
        assert_eq!(
            parsed_port("COLOR=Eb4034 RoLe=InPuT").expect("parses"),
            (Some([0xeb, 0x40, 0x34]), Some(crate::PortRole::Input))
        );
        parsed_height("height=d/0").unwrap_err();
        parsed_port("color=12345").unwrap_err();
        parsed_port("role=input role=output").unwrap_err();
        assert_eq!(
            parsed_port("role=multiplex").expect("parses").1,
            Some(crate::PortRole::Multiplex)
        );
        parsed_height("@h=d").unwrap_err();
    }

    #[test]
    fn cube_height_does_not_swallow_the_next_statement() {
        // The offset is optional and `+`/`-` only start one when digits follow.
        let file = parse_ok("BLOG 1.0\n\n0: XZZ [0,0,0] height=d/2\n1: XZZ [-1,0,0]\n0 -> -X\n");
        assert_eq!(file.data_stmts.len(), 3);
    }

    // ---- Keyword tags --------------------------------------------------------

    #[test]
    fn keyword_tags_parse() {
        for kw in ["block", "if", "pipe"] {
            let input = format!("BLOG 1.0\n\n0: XZZ [0,0,0] <{kw}>\n");
            let file = parse_ok(&input);
            let DataStmt::Block(def) = &file.data_stmts[0].node else {
                panic!("expected block");
            };
            assert_eq!(def.tag.as_ref().map(|t| t.node.as_str()), Some(kw));
        }
    }

    #[test]
    fn graph_arrows_set_the_hadamard_flag() {
        let file = parse_ok("BLOG 1.0\n0: XZZ [0,0,0]\n0 -H> +X\n0 -> +Y\n");
        let DataStmt::Pipe(def) = &file.data_stmts.last().expect("pipe stmt").node else {
            panic!("expected pipe");
        };
        assert!(!def.hadamard);
        let DataStmt::Pipe(def) = &file.data_stmts[1].node else {
            panic!("expected pipe");
        };
        assert!(def.hadamard);

        parse("BLOG 1.0\n0: walk XZZ [0,0,0] -H> [1,0,1]\n").unwrap_err();
    }

    // ---- AST shape (parse without lowering) ---------------------------------

    use crate::SelectiveKind;
    use crate::parser::parse_blog_to_ast;

    #[test]
    fn test_case_insensitive_keywords() {
        for input in [
            "BLOG 1.0\n0: walk XZZ [0,0,0] -> [0,0,1]",
            "blog 1.0\n0: WaLk XZZ [0,0,0] -> [0,0,1]",
        ] {
            let ast = parse_blog_to_ast(input).expect("case-insensitive keywords should parse");
            assert_eq!(ast.data_stmts.len(), 1, "failed for: {input}");
        }
    }

    #[test]
    fn test_comment_handling() {
        let input = "BLOG 1.0\n# comment\n\n0: XZZ [0,0,0] <a#b> # trailing\n";
        let ast = parse_blog_to_ast(input).expect("comments should be ignored during parsing");
        assert_eq!(ast.data_stmts.len(), 1);
    }

    #[test]
    fn test_measure_target_ast_uses_node_and_edge_variants() {
        let input = "BLOG 1.0\n5: XZZ [0,0,0]\nmxy0 = measure 5\nmzz = measure 5 -> +X";
        let ast = parse_blog_to_ast(input).expect("measure AST should parse");
        assert_eq!(ast.action_stmts.len(), 2);

        let crate::parser::ast::ActionStmt::Measure(first) = &ast.action_stmts[0].node else {
            panic!("expected first measure");
        };
        assert!(matches!(
            first.target.node,
            crate::parser::ast::MeasureTargetAst::Node(_)
        ));

        let crate::parser::ast::ActionStmt::Measure(second) = &ast.action_stmts[1].node else {
            panic!("expected second measure");
        };
        assert!(matches!(
            second.target.node,
            crate::parser::ast::MeasureTargetAst::Edge(_, _)
        ));
    }

    #[test]
    fn sectionless_syntax_dispatches_by_delimiter() {
        let ast = parse_blog_to_ast(
            "BLOG 1.0\n\
             0: ZXZ [0, 0, 0]\n\
             0 -H> +Z\n\
             m = measure 0 -> +Z\n\
             x = !m\n\
             resolve 0 if x\n",
        )
        .unwrap();

        assert_eq!(ast.data_stmts.len(), 2);
        assert_eq!(ast.action_stmts.len(), 3);
    }

    #[test]
    fn test_selective_block_reversed_pair_is_canonicalized_in_ast() {
        let ast = parse_blog_to_ast("BLOG 1.0\n0: YX [0,0,0]").unwrap();
        let crate::parser::ast::DataStmt::Block(block) = &ast.data_stmts[0].node else {
            panic!("expected block");
        };
        assert_eq!(
            block.kind.node,
            crate::parser::ast::BlockKindAst::Concrete(BlockKind::Selective(SelectiveKind::XY))
        );
        assert!(block.selective_order_reversed);
    }

    #[test]
    fn test_selective_block_forward_pair_is_not_marked_reversed() {
        let ast = parse_blog_to_ast("BLOG 1.0\n0: XZ [0,0,0]").unwrap();
        let crate::parser::ast::DataStmt::Block(block) = &ast.data_stmts[0].node else {
            panic!("expected block");
        };
        assert_eq!(
            block.kind.node,
            crate::parser::ast::BlockKindAst::Concrete(BlockKind::Selective(SelectiveKind::XZ))
        );
        assert!(!block.selective_order_reversed);
    }

    #[test]
    fn test_selective_block_repeated_basis_token_is_rejected() {
        let err = parse_blog_to_ast("BLOG 1.0\n0: XX [0,0,0]").expect_err("XX should be rejected");
        assert!(matches!(err, ParseError::Syntax { .. }));
    }

    #[test]
    fn branch_braces_are_line_delimited() {
        let source = "BLOG 1.0\nbranch b {\n  false {\n    1: X [0,0,1]\n  }\n  true {\n    2: Z [0,0,1]\n  }\n}\n";
        assert!(matches!(
            parse_ok(source).data_stmts[0].node,
            DataStmt::Branch(_)
        ));
        parse("BLOG 1.0\nbranch b { false { } true { } }\n").unwrap_err();
    }

    #[test]
    fn module_statements_interleave_and_arrows_map_to_ast() {
        let source = "BLOG 1.0\n\
module main {\n\
  in enable\n\
  a: Child @ [0,0,0]\n\
  0: Port [0,0,-1]\n\
  out done = enable\n\
  0 -> a.q\n\
  in q: bit = 0\n\
  b: Child @ [1,0,0]\n\
  1: Port [0,0,1]\n\
  0 -> 1\n\
  a.q -> 1\n\
  a.q -H> b.q\n\
  enable => a.enable\n\
  a.done => b.enable\n\
}\n";
        let program = parse_program(source).unwrap();
        let module = &program.modules[0].node;
        assert_eq!(module.interface_stmts.len(), 3);
        assert_eq!(module.instance_stmts.len(), 2);
        assert_eq!(module.data_stmts.len(), 3);
        assert_eq!(module.connect_stmts.len(), 5);
    }

    #[test]
    fn program_parser_accepts_only_module_files() {
        parse_program("BLOG 1.0\nmodule = value\n").unwrap_err();
        parse_program("BLOG 1.0\nmodule main {\n}\n").unwrap();
        parse_program("BLOG 1.0\nimport\"child.blog\"as Child\n").unwrap();
    }

    #[test]
    fn malformed_program_statements_are_rejected() {
        for statement in [
            "module Broken",
            "import child.blog",
            "root main",
            "module = value",
            "import = value",
        ] {
            parse_program(&format!("BLOG 1.0\n{statement}\n")).unwrap_err();
        }
    }

    #[test]
    fn block_member_bindings_accept_hadamard_arrow() {
        for connection in ["0 -H> child.q", "child.q -H> 0"] {
            let source = format!("BLOG 1.0\nmodule main {{\n  {connection}\n}}\n");
            let parsed = parse_program(&source).unwrap();
            assert!(matches!(
                parsed.modules[0].node.connect_stmts[0].node,
                ConnectStmt::Bind { hadamard: true, .. }
            ));
        }
    }
}
