/// Mimi compiler error codes.
///
/// Code ranges:
/// - E0001-E0099: Lexical errors (illegal character, unterminated string, etc.)
/// - E0100-E0199: Syntax errors (missing token, wrong structure, etc.)
/// - E0200-E0299: Type errors (type mismatch, missing trait implementation, etc.)
/// - E0300-E0399: Ownership/borrow errors
/// - E0400-E0499: Semantic errors (undefined variable, duplicate definition, etc.)
/// - E0500-E0599: Contract/intention errors (lock conflicts, etc.)
/// - E0600-E0699: Warnings
///
/// Parse error codes (E00xx - E01xx).
pub const E0001: &str = "E0001"; // unterminated string literal
pub const E0002: &str = "E0002"; // unterminated f-string
pub const E0003: &str = "E0003"; // illegal character
pub const E0004: &str = "E0004"; // unexpected end of file
pub const E0005: &str = "E0005"; // invalid integer literal
pub const E0006: &str = "E0006"; // invalid float literal
pub const E0007: &str = "E0007"; // tab indentation not allowed

pub const E0100: &str = "E0100"; // expected token, found other
pub const E0101: &str = "E0101"; // expected ';', found '}'
pub const E0102: &str = "E0102"; // unexpected token at top level
pub const E0103: &str = "E0103"; // unexpected token in pattern
pub const E0104: &str = "E0104"; // unexpected token in expression
pub const E0105: &str = "E0105"; // unexpected token in statement
pub const E0106: &str = "E0106"; // missing '{' for block
pub const E0107: &str = "E0107"; // missing '}' to close block
pub const E0108: &str = "E0108"; // missing '(' for function call
pub const E0109: &str = "E0109"; // missing ')' to close call
pub const E0110: &str = "E0110"; // missing '->' for return type
pub const E0111: &str = "E0111"; // missing ':' for type annotation
pub const E0112: &str = "E0112"; // missing identifier
pub const E0113: &str = "E0113"; // missing string literal
pub const E0114: &str = "E0114"; // missing '=>' for match arm
pub const E0115: &str = "E0115"; // missing ',' between elements
pub const E0116: &str = "E0116"; // missing 'in' in for loop
pub const E0117: &str = "E0117"; // missing 'else' block
pub const E0118: &str = "E0118"; // unterminated interpolation in f-string
pub const E0119: &str = "E0119"; // `...` placeholder not allowed in production mode
pub const E0120: &str = "E0120"; // unexpected '$' character

/// Type error codes (E02xx)
pub const E0200: &str = "E0200"; // type mismatch
pub const E0201: &str = "E0201"; // cannot negate type
pub const E0202: &str = "E0202"; // cannot apply operator to types
pub const E0203: &str = "E0203"; // cannot apply ! to type
pub const E0204: &str = "E0204"; // cannot dereference type
pub const E0205: &str = "E0205"; // if condition must be bool
pub const E0206: &str = "E0206"; // while condition must be bool
pub const E0207: &str = "E0207"; // return type mismatch
pub const E0208: &str = "E0208"; // cannot assign to immutable variable
pub const E0209: &str = "E0209"; // cannot assign type to variable of different type
pub const E0210: &str = "E0210"; // function expects N arguments, found M
pub const E0211: &str = "E0211"; // argument type mismatch
pub const E0212: &str = "E0212"; // for loop requires a List
pub const E0213: &str = "E0213"; // match expression must have at least one arm
pub const E0214: &str = "E0214"; // match arm body type mismatch
pub const E0215: &str = "E0215"; // match is not exhaustive
pub const E0216: &str = "E0216"; // match guard must be bool
pub const E0217: &str = "E0217"; // index must be integer
pub const E0218: &str = "E0218"; // cannot index type
pub const E0219: &str = "E0219"; // field access requires record type
pub const E0220: &str = "E0220"; // type has no field
pub const E0221: &str = "E0221"; // type has no method
pub const E0222: &str = "E0222"; // method call requires named type
pub const E0223: &str = "E0223"; // callee must be function name
pub const E0224: &str = "E0224"; // cannot apply ? to type
pub const E0225: &str = "E0225"; // pattern type does not match subject
pub const E0226: &str = "E0226"; // constructor undefined
pub const E0227: &str = "E0227"; // variant takes no arguments
pub const E0228: &str = "E0228"; // variant expects N arguments, found M
pub const E0229: &str = "E0229"; // list element type mismatch
pub const E0230: &str = "E0230"; // comprehension guard must be bool
pub const E0231: &str = "E0231"; // unknown type
pub const E0232: &str = "E0232"; // list element type mismatch
pub const E0233: &str = "E0233"; // cannot assign through non-mutable reference
pub const E0234: &str = "E0234"; // missing return value
pub const E0235: &str = "E0235"; // function does not return on all paths
pub const E0236: &str = "E0236"; // unreachable statement after return
pub const E0237: &str = "E0237"; // division by zero literal
pub const E0238: &str = "E0238"; // modulo by zero literal
pub const E0239: &str = "E0239"; // turbofish type argument count mismatch
pub const E0240: &str = "E0240"; // where constraint violated
pub const E0241: &str = "E0241"; // effect not available

/// Ownership/borrow error codes (E03xx)
pub const E0300: &str = "E0300"; // cannot borrow as mutable because already immutably borrowed
pub const E0301: &str = "E0301"; // cannot borrow as mutable because already mutably borrowed
pub const E0302: &str = "E0302"; // cannot borrow as immutable because already mutably borrowed
pub const E0303: &str = "E0303"; // capability must be consumed before end of scope
pub const E0304: &str = "E0304"; // capability already consumed
pub const E0305: &str = "E0305"; // cannot capture local_shared in parasteps

/// Semantic error codes (E04xx)
pub const E0400: &str = "E0400"; // undefined variable
pub const E0401: &str = "E0401"; // undefined function
pub const E0402: &str = "E0402"; // duplicate definition
pub const E0403: &str = "E0403"; // variable shadows outer variable
pub const E0404: &str = "E0404"; // break outside of loop
pub const E0405: &str = "E0405"; // continue outside of loop
pub const E0406: &str = "E0406"; // undefined trait
pub const E0407: &str = "E0407"; // undefined type
pub const E0408: &str = "E0408"; // missing method in trait impl
pub const E0409: &str = "E0409"; // type alias cycle
pub const E0410: &str = "E0410"; // cannot infer record type without explicit type name
pub const E0411: &str = "E0411"; // weak requires a shared value

/// Contract/intention error codes (E05xx)
pub const E0500: &str = "E0500"; // cannot modify $-locked fragment
pub const E0501: &str = "E0501"; // strict mode: contract modifications not allowed

/// Warning codes (E06xx)
pub const E0600: &str = "E0600"; // variable shadows outer variable (warning)
pub const E0601: &str = "E0601"; // unused variable
pub const E0602: &str = "E0602"; // unused import

/// Get a human-readable description for an error code.
pub fn describe(code: &str) -> &'static str {
    match code {
        E0001 => "unterminated string literal",
        E0002 => "unterminated f-string",
        E0003 => "illegal character",
        E0004 => "unexpected end of file",
        E0005 => "invalid integer literal",
        E0006 => "invalid float literal",
        E0007 => "tab indentation not allowed",

        E0100 => "expected token",
        E0101 => "expected ';'",
        E0102 => "unexpected token at top level",
        E0103 => "unexpected token in pattern",
        E0104 => "unexpected token in expression",
        E0105 => "unexpected token in statement",
        E0106 => "missing '{' for block",
        E0107 => "missing '}' to close block",
        E0108 => "missing '(' for function call",
        E0109 => "missing ')' to close call",
        E0110 => "missing '->' for return type",
        E0111 => "missing ':' for type annotation",
        E0112 => "missing identifier",
        E0113 => "missing string literal",
        E0114 => "missing '=>' for match arm",
        E0115 => "missing ',' between elements",
        E0116 => "missing 'in' in for loop",
        E0117 => "missing 'else' block",
        E0118 => "unterminated interpolation in f-string",
        E0119 => "'...' placeholder not allowed in production mode",
        E0120 => "unexpected '$' character",

        E0200 => "type mismatch",
        E0201 => "cannot negate type",
        E0202 => "cannot apply operator to types",
        E0203 => "cannot apply '!' to type",
        E0204 => "cannot dereference type",
        E0205 => "if condition must be bool",
        E0206 => "while condition must be bool",
        E0207 => "return type mismatch",
        E0208 => "cannot assign to immutable variable",
        E0209 => "cannot assign type to variable of different type",
        E0210 => "function argument count mismatch",
        E0211 => "argument type mismatch",
        E0212 => "for loop requires a List",
        E0213 => "match expression must have at least one arm",
        E0214 => "match arm body type mismatch",
        E0215 => "match is not exhaustive",
        E0216 => "match guard must be bool",
        E0217 => "index must be integer",
        E0218 => "cannot index type",
        E0219 => "field access requires record type",
        E0220 => "type has no field",
        E0221 => "type has no method",
        E0222 => "method call requires named type",
        E0223 => "callee must be function name",
        E0224 => "cannot apply '?' to type",
        E0225 => "pattern type does not match subject",
        E0226 => "constructor undefined",
        E0227 => "variant takes no arguments",
        E0228 => "variant argument count mismatch",
        E0229 => "list element type mismatch",
        E0230 => "comprehension guard must be bool",
        E0231 => "unknown type",
        E0232 => "list element type mismatch",
        E0233 => "cannot assign through non-mutable reference",
        E0234 => "missing return value",
        E0235 => "function does not return on all paths",
        E0236 => "unreachable statement after return",
        E0237 => "division by zero literal",
        E0238 => "modulo by zero literal",
        E0239 => "turbofish type argument count mismatch",
        E0240 => "where constraint violated",
        E0241 => "effect not available",

        E0300 => "cannot borrow as mutable because already immutably borrowed",
        E0301 => "cannot borrow as mutable because already mutably borrowed",
        E0302 => "cannot borrow as immutable because already mutably borrowed",
        E0303 => "capability must be consumed before end of scope",
        E0304 => "capability already consumed",
        E0305 => "cannot capture local_shared in parasteps",

        E0400 => "undefined variable",
        E0401 => "undefined function",
        E0402 => "duplicate definition",
        E0403 => "variable shadows outer variable",
        E0404 => "break outside of loop",
        E0405 => "continue outside of loop",
        E0406 => "undefined trait",
        E0407 => "undefined type",
        E0408 => "missing method in trait impl",
        E0409 => "type alias cycle",
        E0410 => "cannot infer record type without explicit type name",
        E0411 => "weak requires a shared value",

        E0500 => "cannot modify $-locked fragment",
        E0501 => "strict mode: contract modifications not allowed",

        E0600 => "variable shadows outer variable",
        E0601 => "unused variable",
        E0602 => "unused import",

        _ => "unknown error",
    }
}
