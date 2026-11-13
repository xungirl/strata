//! The original Porter (1980) stemming algorithm, as implemented by Lucene's
//! `PorterStemmer` (which `PorterStemFilter` and therefore Anserini use).
//!
//! Why have our own when `rust-stemmers` exists? `rust-stemmers` only offers
//! Snowball English ("Porter2"), a later revision that stems some words
//! differently. To reproduce Anserini's published BM25 numbers we need the
//! exact same terms, so this module ports Lucene's Java class step by step,
//! including Martin Porter's two documented departures from the paper
//! (`-bli` -> `-ble` instead of `-abli` -> `-able`, and the extra
//! `-logi` -> `-log` rule) and Lucene's "Bug 1" guards.
//!
//! It is validated against Porter's official 23,531-word vocabulary and its
//! expected output (`testdata/porter_*.txt`, from
//! <https://tartarus.org/martin/PorterStemmer/>).
//!
//! The algorithm is a sequence of suffix-rewriting steps. Most rules only
//! fire when the remaining stem is "long enough", measured by `m()`: the
//! number of vowel-consonant sequences. Writing a word as `[C](VC)^m[V]`
//! (C = run of consonants, V = run of vowels):
//!
//! ```text
//! m = 0: tr, ee, tree      m = 1: trouble, oats, ivy      m = 2: troubles, private
//! ```
//!
//! The code below keeps the Java names (`cons`, `m`, `ends`, `setto`, `r`,
//! `step1`..`step6`) so it can be compared side by side with the original.

/// Mutable working state for stemming one word.
///
/// Lucene works on a `char[]` buffer with indices `k` (last char of the
/// current word) and `j` (end of the stem before a matched suffix); we do the
/// same on a `Vec<char>`. Non-ASCII letters are simply consonants, as in Java.
struct Stemmer {
    b: Vec<char>,
    /// Index of the last character of the current word.
    k: usize,
    /// Set by `ends()`: index of the last character *before* the suffix.
    /// Signed because it can be -1 when the suffix is the whole word.
    j: isize,
}

impl Stemmer {
    /// Is `b[i]` a consonant? `y` is a consonant at the start of a word or
    /// after a vowel ("toy": y is a consonant; "syzygy": vowel).
    fn cons(&self, i: usize) -> bool {
        match self.b[i] {
            'a' | 'e' | 'i' | 'o' | 'u' => false,
            'y' => i == 0 || !self.cons(i - 1),
            _ => true,
        }
    }

    /// Number of VC sequences in `b[0..=j]`.
    fn m(&self) -> usize {
        let j = self.j;
        let mut n = 0;
        let mut i: isize = 0;
        // Skip the optional leading consonants.
        loop {
            if i > j {
                return n;
            }
            if !self.cons(i as usize) {
                break;
            }
            i += 1;
        }
        i += 1;
        loop {
            // Skip vowels...
            loop {
                if i > j {
                    return n;
                }
                if self.cons(i as usize) {
                    break;
                }
                i += 1;
            }
            i += 1;
            n += 1; // ...a vowel run followed by a consonant: one VC.
                    // ...then skip consonants.
            loop {
                if i > j {
                    return n;
                }
                if !self.cons(i as usize) {
                    break;
                }
                i += 1;
            }
            i += 1;
        }
    }

    /// Does `b[0..=j]` contain a vowel?
    fn vowel_in_stem(&self) -> bool {
        (0..=self.j).any(|i| !self.cons(i as usize))
    }

    /// Do `b[j-1]` and `b[j]` form a double consonant?
    fn double_c(&self, j: usize) -> bool {
        j >= 1 && self.b[j] == self.b[j - 1] && self.cons(j)
    }

    /// Is `b[i-2..=i]` consonant-vowel-consonant, with the last consonant not
    /// w, x or y? Used to restore an `e` ("hop" + "ing" -> "hope"? no: cvc
    /// tells "hop" ends like "hope" would, so "hoping" -> "hope").
    fn cvc(&self, i: isize) -> bool {
        if i < 2 {
            return false;
        }
        let i = i as usize;
        if !self.cons(i) || self.cons(i - 1) || !self.cons(i - 2) {
            return false;
        }
        !matches!(self.b[i], 'w' | 'x' | 'y')
    }

    /// Does the word `b[0..=k]` end with `s`? If so, set `j` to just before it.
    fn ends(&mut self, s: &str) -> bool {
        let len = s.chars().count();
        if len > self.k + 1 {
            return false;
        }
        let start = self.k + 1 - len;
        if !self.b[start..=self.k].iter().copied().eq(s.chars()) {
            return false;
        }
        self.j = start as isize - 1;
        true
    }

    /// Replace `b[j+1..=k]` with `s` and move `k` to the new end.
    fn setto(&mut self, s: &str) {
        let start = (self.j + 1) as usize;
        self.b.truncate(start);
        self.b.extend(s.chars());
        self.k = start + s.chars().count() - 1;
        // Keep the buffer exactly the word, so indices beyond k never matter.
    }

    /// `setto(s)` if the stem before the suffix has `m() > 0`.
    fn r(&mut self, s: &str) {
        if self.m() > 0 {
            self.setto(s);
        }
    }

    /// Plurals and -ed / -ing: caresses -> caress, ponies -> poni,
    /// cats -> cat, agreed -> agree, plastered -> plaster, hopping -> hop.
    fn step1(&mut self) {
        if self.b[self.k] == 's' {
            if self.ends("sses") {
                self.k -= 2;
            } else if self.ends("ies") {
                self.setto("i");
            } else if self.b[self.k - 1] != 's' {
                self.k -= 1;
            }
        }
        if self.ends("eed") {
            if self.m() > 0 {
                self.k -= 1;
            }
        } else if (self.ends("ed") || self.ends("ing")) && self.vowel_in_stem() {
            self.k = self.j as usize;
            if self.ends("at") {
                self.setto("ate");
            } else if self.ends("bl") {
                self.setto("ble");
            } else if self.ends("iz") {
                self.setto("ize");
            } else if self.double_c(self.k) {
                let ch = self.b[self.k];
                if !matches!(ch, 'l' | 's' | 'z') {
                    self.k -= 1;
                }
            } else {
                // `m()` reads `j`; after the checks above `j` points at the
                // end of the shortened word, as in the Java code.
                self.j = self.k as isize;
                if self.m() == 1 && self.cvc(self.k as isize) {
                    self.setto("e");
                }
            }
        }
    }

    /// Terminal y -> i when there is another vowel in the stem: happy -> happi.
    fn step2(&mut self) {
        if self.ends("y") && self.vowel_in_stem() {
            self.b[self.k] = 'i';
        }
    }

    /// Double suffixes to single ones: -ational -> -ate, -iveness -> -ive, ...
    fn step3(&mut self) {
        if self.k == 0 {
            return;
        }
        let rules: &[(&str, &str)] = match self.b[self.k - 1] {
            'a' => &[("ational", "ate"), ("tional", "tion")],
            'c' => &[("enci", "ence"), ("anci", "ance")],
            'e' => &[("izer", "ize")],
            'l' => &[
                ("bli", "ble"),
                ("alli", "al"),
                ("entli", "ent"),
                ("eli", "e"),
                ("ousli", "ous"),
            ],
            'o' => &[("ization", "ize"), ("ation", "ate"), ("ator", "ate")],
            's' => &[
                ("alism", "al"),
                ("iveness", "ive"),
                ("fulness", "ful"),
                ("ousness", "ous"),
            ],
            't' => &[("aliti", "al"), ("iviti", "ive"), ("biliti", "ble")],
            'g' => &[("logi", "log")],
            _ => &[],
        };
        self.apply_first(rules);
    }

    /// -ic-, -full, -ness etc.: -icate -> -ic, -ative -> "", -ness -> "".
    fn step4(&mut self) {
        let rules: &[(&str, &str)] = match self.b[self.k] {
            'e' => &[("icate", "ic"), ("ative", ""), ("alize", "al")],
            'i' => &[("iciti", "ic")],
            'l' => &[("ical", "ic"), ("ful", "")],
            's' => &[("ness", "")],
            _ => &[],
        };
        self.apply_first(rules);
    }

    /// For the first rule whose suffix matches, apply `r(replacement)` and
    /// stop (even if `m()` was too small to rewrite), like the Java `break`s.
    fn apply_first(&mut self, rules: &[(&str, &str)]) {
        for (suffix, replacement) in rules {
            if self.ends(suffix) {
                self.r(replacement);
                return;
            }
        }
    }

    /// Remove -ant, -ence, -ment, ... when the stem has `m() > 1`.
    fn step5(&mut self) {
        if self.k == 0 {
            return;
        }
        let suffixes: &[&str] = match self.b[self.k - 1] {
            'a' => &["al"],
            'c' => &["ance", "ence"],
            'e' => &["er"],
            'i' => &["ic"],
            'l' => &["able", "ible"],
            'n' => &["ant", "ement", "ment", "ent"],
            'o' => {
                // -ion only after s or t (adoption -> adopt); also -ou(s).
                let ion =
                    self.ends("ion") && self.j >= 0 && matches!(self.b[self.j as usize], 's' | 't');
                if !ion && !self.ends("ou") {
                    return;
                }
                &[]
            }
            's' => &["ism"],
            't' => &["ate", "iti"],
            'u' => &["ous"],
            'v' => &["ive"],
            'z' => &["ize"],
            _ => return,
        };
        // For the 'o' case `ends` has already set `j`; otherwise find a match.
        if !suffixes.is_empty() && !suffixes.iter().any(|s| self.ends(s)) {
            return;
        }
        if self.m() > 1 {
            self.k = self.j as usize;
        }
    }

    /// Remove a final -e (probate -> probat, rate stays) and reduce -ll to -l
    /// when the stem is long: controll -> control.
    fn step6(&mut self) {
        self.j = self.k as isize;
        if self.b[self.k] == 'e' {
            let a = self.m();
            if a > 1 || (a == 1 && !self.cvc(self.k as isize - 1)) {
                self.k -= 1;
            }
        }
        if self.b[self.k] == 'l' && self.double_c(self.k) && self.m() > 1 {
            self.k -= 1;
        }
    }
}

/// Stem a lowercase word with the classic Porter algorithm (Lucene flavour).
/// Words of one or two characters are returned unchanged.
pub fn porter_stem(word: &str) -> String {
    let b: Vec<char> = word.chars().collect();
    if b.len() <= 2 {
        return word.to_string();
    }
    let mut s = Stemmer {
        k: b.len() - 1,
        b,
        j: 0,
    };
    s.step1();
    s.step2();
    s.step3();
    s.step4();
    s.step5();
    s.step6();
    s.b[..=s.k].iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn examples_from_porters_paper() {
        let cases = [
            ("caresses", "caress"),
            ("ponies", "poni"),
            ("cats", "cat"),
            ("feed", "feed"),
            ("agreed", "agre"),
            ("plastered", "plaster"),
            ("motoring", "motor"),
            ("sing", "sing"),
            ("hopping", "hop"),
            ("falling", "fall"),
            ("filing", "file"),
            ("happy", "happi"),
            ("relational", "relat"),
            ("generalization", "gener"),
            ("adoption", "adopt"),
            ("controll", "control"),
            ("generously", "gener"),
        ];
        for (word, stem) in cases {
            assert_eq!(porter_stem(word), stem, "{word}");
        }
    }

    #[test]
    fn short_and_non_ascii_words() {
        assert_eq!(porter_stem("a"), "a");
        assert_eq!(porter_stem("is"), "is");
        assert_eq!(porter_stem("3.14"), "3.14");
        assert_eq!(porter_stem("cafés"), "café");
    }

    /// Porter's official vocabulary: 23,531 words with expected stems.
    #[test]
    fn matches_official_vocabulary() {
        let voc = include_str!("../testdata/porter_voc.txt");
        let out = include_str!("../testdata/porter_output.txt");
        let mut mismatches = Vec::new();
        let mut n = 0;
        for (word, expected) in voc.lines().zip(out.lines()) {
            n += 1;
            let got = porter_stem(word);
            if got != expected {
                mismatches.push(format!("{word}: got {got}, expected {expected}"));
            }
        }
        assert_eq!(n, 23_531);
        assert!(
            mismatches.is_empty(),
            "{} mismatches, first: {:?}",
            mismatches.len(),
            &mismatches[..mismatches.len().min(10)]
        );
    }
}
