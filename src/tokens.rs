//! A tokenizer approximation, so a token budget (C5, B8) is counted without a model's
//! vocabulary: an ASCII word costs a token per 4 chars, any other visible char one.

#[must_use]
pub fn count(s: &str) -> usize {
    let (mut n, mut run) = (0, 0_usize);
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            run += 1;
            continue;
        }
        n += run.div_ceil(4);
        run = 0;
        n += usize::from(!c.is_whitespace());
    }
    n + run.div_ceil(4)
}

#[cfg(test)]
mod tests {
    use super::count;

    #[test]
    fn words_and_punctuation() {
        assert_eq!(count(""), 0);
        assert_eq!(count("brain find"), 3);
        assert_eq!(count("`brain find`"), 5);
        assert_eq!(count("arxiv:2410.24164"), 2 + 1 + 1 + 1 + 2);
    }
}
