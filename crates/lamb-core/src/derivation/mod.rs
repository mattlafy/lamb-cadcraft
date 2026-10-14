//! LAMB's derivation engine, ported from `packages/derivation-engine/src/` in LAMB. It reads the
//! drawing only through [`read::ProjectRead`], so CADCraft's adapter can implement the same trait
//! over its own store. Design: LAMB's `docs/07-architecture/lamb-core-derivation-port.md`.

pub mod alignment;
pub mod detail_tags;
pub mod issues;
pub mod linking;
pub mod markers;
pub mod projection;
pub mod proposals;
pub mod read;
pub mod reference;
pub mod resolve;
pub mod solid_cut_lines;
pub mod solids;
pub mod synthesis;
pub mod tilt;
pub mod travel;
pub mod underlay;
pub mod wall_junctions;
pub mod wall_openings;
pub mod wall_spaces;
pub mod walls;
pub mod write;

/// JS `Number.prototype.toFixed(digits)`: rounds the exact binary value half away from zero (a bare
/// `{:.6}` rounds ties to even), and prints a minus sign iff `x < 0.0`, so `-0.0` gives
/// `"0.000000"` and `-1e-9` gives `"-0.000000"`. Like JS, `|x| >= 1e21` and non-finite values fall
/// back to the plain number string.
pub(crate) fn js_to_fixed(x: f64, digits: usize) -> String {
    if !x.is_finite() {
        return if x.is_nan() {
            "NaN".into()
        } else if x > 0.0 {
            "Infinity".into()
        } else {
            "-Infinity".into()
        };
    }
    if x.abs() >= 1e21 {
        return format!("{x:e}");
    }
    // Every finite f64 has at most 1074 fractional binary digits, so 1100 decimal places print it
    // exactly; rounding is then done by hand on the exact digits.
    let exact = format!("{:.1100}", x.abs());
    let (int_part, frac_part) = exact.split_once('.').unwrap_or((&exact, ""));
    let mut digits_vec: Vec<u8> = int_part.bytes().chain(frac_part.bytes().take(digits)).map(|b| b - b'0').collect();
    let round_up = frac_part.as_bytes().get(digits).is_some_and(|&b| b >= b'5');
    if round_up {
        let mut i = digits_vec.len();
        loop {
            if i == 0 {
                digits_vec.insert(0, 1);
                break;
            }
            i -= 1;
            if digits_vec[i] == 9 {
                digits_vec[i] = 0;
            } else {
                digits_vec[i] += 1;
                break;
            }
        }
    }
    let split = digits_vec.len() - digits;
    let mut out = String::with_capacity(digits_vec.len() + 2);
    if x < 0.0 {
        out.push('-');
    }
    out.extend(digits_vec[..split].iter().map(|d| char::from(b'0' + d)));
    if digits > 0 {
        out.push('.');
        out.extend(digits_vec[split..].iter().map(|d| char::from(b'0' + d)));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::js_to_fixed;

    #[test]
    fn to_fixed_rounds_exact_ties_away_from_zero() {
        assert_eq!(js_to_fixed(0.0078125, 6), "0.007813");
        assert_eq!(format!("{:.6}", 0.0078125), "0.007812");
        assert_eq!(js_to_fixed(-0.0078125, 6), "-0.007813");
        assert_eq!(js_to_fixed(9.9999999, 6), "10.000000");
        assert_eq!(js_to_fixed(0.5, 0), "1");
        assert_eq!(js_to_fixed(1.0, 6), "1.000000");
    }

    #[test]
    fn to_fixed_negative_zero_has_no_sign() {
        assert_eq!(js_to_fixed(-0.0, 6), "0.000000");
    }

    #[test]
    fn to_fixed_tiny_negative_keeps_sign() {
        assert_eq!(js_to_fixed(-1e-9, 6), "-0.000000");
    }
}
