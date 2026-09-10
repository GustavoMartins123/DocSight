#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Diacritic {
    Acute,
    Grave,
    Circumflex,
    Tilde,
    Diaeresis,
    Ring,
    Cedilla,
}

impl Diacritic {
    fn mark_rows(self) -> [u8; 2] {
        match self {
            Self::Acute => [4, 2],
            Self::Grave => [2, 4],
            Self::Circumflex | Self::Tilde | Self::Diaeresis | Self::Ring => [10, 0],
            Self::Cedilla => [0, 0],
        }
    }
}

pub fn latin_decomposition(character: char) -> Option<(char, Diacritic)> {
    use Diacritic::{Acute, Cedilla, Circumflex, Diaeresis, Grave, Ring, Tilde};
    Some(match character {
        'À' => ('A', Grave),
        'Á' => ('A', Acute),
        'Â' => ('A', Circumflex),
        'Ã' => ('A', Tilde),
        'Ä' => ('A', Diaeresis),
        'Å' => ('A', Ring),
        'Ç' => ('C', Cedilla),
        'È' => ('E', Grave),
        'É' => ('E', Acute),
        'Ê' => ('E', Circumflex),
        'Ë' => ('E', Diaeresis),
        'Ì' => ('I', Grave),
        'Í' => ('I', Acute),
        'Î' => ('I', Circumflex),
        'Ï' => ('I', Diaeresis),
        'Ñ' => ('N', Tilde),
        'Ò' => ('O', Grave),
        'Ó' => ('O', Acute),
        'Ô' => ('O', Circumflex),
        'Õ' => ('O', Tilde),
        'Ö' => ('O', Diaeresis),
        'Ù' => ('U', Grave),
        'Ú' => ('U', Acute),
        'Û' => ('U', Circumflex),
        'Ü' => ('U', Diaeresis),
        'Ý' => ('Y', Acute),
        'à' => ('a', Grave),
        'á' => ('a', Acute),
        'â' => ('a', Circumflex),
        'ã' => ('a', Tilde),
        'ä' => ('a', Diaeresis),
        'å' => ('a', Ring),
        'ç' => ('c', Cedilla),
        'è' => ('e', Grave),
        'é' => ('e', Acute),
        'ê' => ('e', Circumflex),
        'ë' => ('e', Diaeresis),
        'ì' => ('i', Grave),
        'í' => ('i', Acute),
        'î' => ('i', Circumflex),
        'ï' => ('i', Diaeresis),
        'ñ' => ('n', Tilde),
        'ò' => ('o', Grave),
        'ó' => ('o', Acute),
        'ô' => ('o', Circumflex),
        'õ' => ('o', Tilde),
        'ö' => ('o', Diaeresis),
        'ù' => ('u', Grave),
        'ú' => ('u', Acute),
        'û' => ('u', Circumflex),
        'ü' => ('u', Diaeresis),
        'ý' | 'ÿ' => ('y', Acute),
        _ => return None,
    })
}

pub fn compose_glyph(base: [u8; 7], diacritic: Diacritic, uppercase: bool) -> [u8; 7] {
    if diacritic == Diacritic::Cedilla {
        let mut composed = base;
        composed[6] = 8;
        return composed;
    }
    let mark = diacritic.mark_rows();
    let tail = if uppercase {
        [base[0], base[2], base[3], base[5], base[6]]
    } else {
        [base[2], base[3], base[4], base[5], base[6]]
    };
    [
        mark[0], mark[1], tail[0], tail[1], tail[2], tail[3], tail[4],
    ]
}

pub fn is_combining_mark(character: char) -> bool {
    matches!(character, '\u{0300}'..='\u{036f}')
}
