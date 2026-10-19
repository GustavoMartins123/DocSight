use crate::DocsightError;

pub fn format_ooxml_number(mut number: u32, format: Option<&str>) -> Result<String, DocsightError> {
    let format = format.unwrap_or("decimal");
    let mut text = match format {
        "decimal" => return Ok(number.to_string()),
        "decimalZero" => return Ok(format!("{number:02}")),
        "upperRoman" | "lowerRoman" if (1..=3999).contains(&number) => {
            let mut text = String::new();
            for (value, label) in [
                (1000, "M"),
                (900, "CM"),
                (500, "D"),
                (400, "CD"),
                (100, "C"),
                (90, "XC"),
                (50, "L"),
                (40, "XL"),
                (10, "X"),
                (9, "IX"),
                (5, "V"),
                (4, "IV"),
                (1, "I"),
            ] {
                while number >= value {
                    text.push_str(label);
                    number -= value;
                }
            }
            text
        }
        "upperLetter" | "lowerLetter" if number > 0 => {
            let index = number - 1;
            let count = index / 26 + 1;
            if count > 1024 {
                return Err(DocsightError::ResourceLimit {
                    resource: "number label characters".to_owned(),
                    limit: 1024,
                });
            }
            let letter = char::from_u32(u32::from(b'A') + index % 26).ok_or_else(|| {
                DocsightError::MalformedDocument {
                    message: "numbering produced an invalid letter".to_owned(),
                }
            })?;
            std::iter::repeat_n(
                letter,
                usize::try_from(count).map_err(|_| DocsightError::ResourceLimit {
                    resource: "number label characters".to_owned(),
                    limit: 1024,
                })?,
            )
            .collect()
        }
        _ => {
            return Err(DocsightError::UnsupportedFeature {
                feature: format!("OOXML numbering format {format} for value {number}"),
            });
        }
    };
    if format.starts_with("lower") {
        text.make_ascii_lowercase();
    }
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn page_and_list_formats_share_exact_labels() -> Result<(), DocsightError> {
        assert_eq!(format_ooxml_number(29, Some("upperLetter"))?, "CC");
        assert_eq!(format_ooxml_number(1649, Some("lowerRoman"))?, "mdcxlix");
        assert_eq!(format_ooxml_number(0, Some("decimalZero"))?, "00");
        assert!(format_ooxml_number(0, Some("upperRoman")).is_err());
        assert!(format_ooxml_number(1, Some("unsupported")).is_err());
        assert!(format_ooxml_number(u32::MAX, Some("upperLetter")).is_err());
        Ok(())
    }
}
