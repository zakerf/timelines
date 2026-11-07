/// Name this binary is built as.
pub const BINARY_NAME: &str = "renderer";

fn main() {
    println!("{BINARY_NAME}: stub, static generation arrives in a later ticket");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binary_name_matches_package() {
        assert_eq!(BINARY_NAME, env!("CARGO_PKG_NAME"));
    }
}
