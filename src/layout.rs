use crate::Result;
use zeroize::Zeroize;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Stroke {
    pub key: u16,
    pub modifiers: u8,
}
pub struct Layout {
    rows: Vec<[u32; 3]>,
}
impl Layout {
    pub fn new(name: &str) -> Result<Self> {
        let source = match name {
            "us" => include_str!("../layouts/us.json"),
            "se" => include_str!("../layouts/se.json"),
            _ => return Err("unsupported keyboard layout"),
        };
        Ok(Self {
            rows: serde_json::from_str(source).map_err(|_| "invalid built-in layout")?,
        })
    }
    pub fn lookup(&self, ch: char) -> Result<Stroke> {
        self.rows
            .binary_search_by_key(&(ch as u32), |r| r[0])
            .map(|i| Stroke {
                key: self.rows[i][1] as u16,
                modifiers: self.rows[i][2] as u8,
            })
            .map_err(|_| "unsupported character for configured layout")
    }
    pub fn validate(&self, data: &[u8]) -> Result<()> {
        let text = std::str::from_utf8(data).map_err(|_| "input is not valid UTF-8")?;
        for mut ch in text.chars() {
            let result = self.lookup(ch);
            ch.zeroize();
            result?;
        }
        Ok(())
    }
    pub fn keys(&self) -> impl Iterator<Item = u16> + '_ {
        self.rows.iter().map(|r| r[1] as u16)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn all_mappings_are_printable_and_supported() {
        for name in ["us", "se"] {
            let l = Layout::new(name).unwrap();
            assert!(l.rows.len() >= 95);
            let mut last = 0;
            for row in &l.rows {
                assert!(row[0] > last);
                last = row[0];
                let c = char::from_u32(row[0]).unwrap();
                assert!(!c.is_control());
                assert_eq!(
                    l.lookup(c).unwrap(),
                    Stroke {
                        key: row[1] as u16,
                        modifiers: row[2] as u8
                    }
                );
                assert!(row[1] > 0 && row[1] < 128);
                assert!(row[2] <= 3);
            }
            for text in [b"\n".as_slice(), b"\t", b"\0", b"\xff", "😀".as_bytes()] {
                assert!(l.validate(text).is_err());
            }
        }
    }
    #[test]
    fn swedish_and_us_physical_positions() {
        let se = Layout::new("se").unwrap();
        let us = Layout::new("us").unwrap();
        se.validate("räksmörgås ÅÄÖ € @ | 🄯".as_bytes()).unwrap();
        assert!(us.validate("å".as_bytes()).is_err());
        assert_eq!(
            se.lookup('å').unwrap(),
            Stroke {
                key: 26,
                modifiers: 0
            }
        );
        assert_eq!(
            se.lookup('Ö').unwrap(),
            Stroke {
                key: 39,
                modifiers: 1
            }
        );
        assert_eq!(
            se.lookup('@').unwrap(),
            Stroke {
                key: 3,
                modifiers: 2
            }
        );
        assert_eq!(
            us.lookup('@').unwrap(),
            Stroke {
                key: 3,
                modifiers: 1
            }
        );
        for b in 32u8..=126 {
            us.lookup(b as char).unwrap();
        }
        // Swedish basic uses dead keys here, which v1 deliberately rejects.
        assert!(se.lookup('^').is_err());
    }
}
