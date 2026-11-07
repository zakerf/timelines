/// Name this binary is built as.
pub const BINARY_NAME: &str = "api";

fn main() {
    println!("{BINARY_NAME}: stub, mutations arrive in a later ticket");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binary_name_matches_package() {
        assert_eq!(BINARY_NAME, env!("CARGO_PKG_NAME"));
    }
}
