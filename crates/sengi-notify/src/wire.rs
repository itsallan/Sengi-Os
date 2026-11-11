//! Reads the two open-ended arguments of `Notify` straight off the wire,
//! keeping only what is used.
//!
//! Taken as `Vec<&str>` and `HashMap<&str, Value>` they would be built in
//! full first: a list of a million actions, or an `image-data` hint whose
//! every byte becomes a boxed value, costs many times the size of the
//! message before a single limit can be applied. Here the first few
//! actions are kept and the rest skipped, and hints that are not read are
//! never materialised.

use std::fmt;

use serde::de::{Deserialize, Deserializer, IgnoredAny, MapAccess, SeqAccess, Visitor};
use zbus::zvariant::{Signature, Type};

use crate::notification::{parse_hints, HintValue, Hints, SCAN_MAX};

/// The `actions` argument, `as`: at most the first [`SCAN_MAX`] pairs.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Actions<'a>(pub Vec<&'a str>);

impl Type for Actions<'_> {
    const SIGNATURE: &'static Signature = <Vec<&str>>::SIGNATURE;
}

impl<'de: 'a, 'a> Deserialize<'de> for Actions<'a> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Strings;

        impl<'de> Visitor<'de> for Strings {
            type Value = Actions<'de>;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a list of strings")
            }

            fn visit_seq<A: SeqAccess<'de>>(self, mut list: A) -> Result<Self::Value, A::Error> {
                let mut kept = Vec::new();
                while kept.len() < SCAN_MAX * 2 {
                    match list.next_element::<&str>()? {
                        Some(text) => kept.push(text),
                        None => return Ok(Actions(kept)),
                    }
                }
                while list.next_element::<IgnoredAny>()?.is_some() {}
                Ok(Actions(kept))
            }
        }

        deserializer.deserialize_seq(Strings)
    }
}

/// The `hints` argument, `a{sv}`, already reduced to the hints we act on.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct WireHints(pub Hints);

impl Type for WireHints {
    const SIGNATURE: &'static Signature =
        <std::collections::HashMap<&str, zbus::zvariant::Value<'_>>>::SIGNATURE;
}

impl<'de> Deserialize<'de> for WireHints {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Dict;

        impl<'de> Visitor<'de> for Dict {
            type Value = WireHints;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a dictionary of hints")
            }

            fn visit_map<A: MapAccess<'de>>(self, mut dict: A) -> Result<Self::Value, A::Error> {
                let mut read: Vec<(&'de str, HintValue<'de>)> = Vec::new();
                while let Some(name) = dict.next_key::<&str>()? {
                    let wanted =
                        matches!(name, "urgency" | "transient" | "category" | "desktop-entry");
                    if wanted && read.len() < SCAN_MAX {
                        read.push((name, dict.next_value::<Variant>()?.0));
                    } else {
                        dict.next_value::<IgnoredAny>()?;
                    }
                }
                Ok(WireHints(parse_hints(read)))
            }
        }

        deserializer.deserialize_map(Dict)
    }
}

/// One variant, kept only if it holds a boolean, an integer or a string.
struct Variant<'a>(HintValue<'a>);

impl<'de: 'a, 'a> Deserialize<'de> for Variant<'a> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Parts;

        impl<'de> Visitor<'de> for Parts {
            type Value = Variant<'de>;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a variant")
            }

            // A variant arrives as its signature followed by its value.
            fn visit_seq<A: SeqAccess<'de>>(self, mut parts: A) -> Result<Self::Value, A::Error> {
                let signature = parts.next_element::<&str>()?.unwrap_or("");
                let value = match signature {
                    "b" | "y" | "n" | "q" | "i" | "u" | "x" | "t" | "s" => parts
                        .next_element::<Scalar>()?
                        .map_or(HintValue::Other, |scalar| scalar.0),
                    _ => {
                        parts.next_element::<IgnoredAny>()?;
                        HintValue::Other
                    }
                };
                Ok(Variant(value))
            }
        }

        deserializer.deserialize_any(Parts)
    }
}

struct Scalar<'a>(HintValue<'a>);

impl<'de: 'a, 'a> Deserialize<'de> for Scalar<'a> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Leaf;

        impl<'de> Visitor<'de> for Leaf {
            type Value = Scalar<'de>;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a boolean, an integer or a string")
            }

            fn visit_bool<E>(self, value: bool) -> Result<Self::Value, E> {
                Ok(Scalar(HintValue::Bool(value)))
            }

            fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E> {
                Ok(Scalar(HintValue::Int(value)))
            }

            fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E> {
                Ok(Scalar(HintValue::Int(
                    i64::try_from(value).unwrap_or(i64::MAX),
                )))
            }

            fn visit_borrowed_str<E>(self, value: &'de str) -> Result<Self::Value, E> {
                Ok(Scalar(HintValue::Str(value)))
            }

            // A string that could not be borrowed from the message.
            fn visit_str<E>(self, _: &str) -> Result<Self::Value, E> {
                Ok(Scalar(HintValue::Other))
            }
        }

        deserializer.deserialize_any(Leaf)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use zbus::zvariant::serialized::{Context, Data};
    use zbus::zvariant::{to_bytes, Value, LE};

    use super::*;
    use crate::notification::Urgency;

    fn hints(map: &HashMap<&str, Value<'_>>) -> Hints {
        let encoded = to_bytes(Context::new_dbus(LE, 0), map).expect("the hints encode");
        let (decoded, _): (WireHints, usize) = encoded.deserialize().expect("the hints decode");
        decoded.0
    }

    fn actions(list: &[&str]) -> Vec<String> {
        let encoded = to_bytes(Context::new_dbus(LE, 0), &list).expect("the actions encode");
        let (decoded, _): (Actions<'_>, usize) = encoded.deserialize().expect("the actions decode");
        decoded.0.into_iter().map(str::to_string).collect()
    }

    #[test]
    fn the_signatures_are_those_of_the_spec() {
        assert_eq!(Actions::SIGNATURE.to_string(), "as");
        assert_eq!(WireHints::SIGNATURE.to_string(), "a{sv}");
    }

    #[test]
    fn the_hints_we_use_are_read() {
        let mut map = HashMap::new();
        map.insert("urgency", Value::U8(2));
        map.insert("transient", Value::Bool(true));
        map.insert("category", Value::from("im.received"));
        map.insert("desktop-entry", Value::from("org.example.Chat"));
        assert_eq!(
            hints(&map),
            Hints {
                urgency: Urgency::Critical,
                transient: true,
                category: "im.received".into(),
                desktop_entry: "org.example.Chat".into(),
            }
        );
        assert_eq!(hints(&HashMap::new()), Hints::default());
    }

    #[test]
    fn integers_of_any_width_are_read() {
        for value in [
            Value::U8(0),
            Value::I16(0),
            Value::U16(0),
            Value::I32(0),
            Value::U32(0),
            Value::I64(0),
            Value::U64(0),
        ] {
            let mut map = HashMap::new();
            map.insert("urgency", value);
            assert_eq!(hints(&map).urgency, Urgency::Low);
        }
        let mut map = HashMap::new();
        map.insert("transient", Value::I32(1));
        assert!(hints(&map).transient);
        map.insert("transient", Value::U64(u64::MAX));
        assert!(hints(&map).transient);
    }

    #[test]
    fn image_data_and_other_hints_are_skipped_whatever_their_shape() {
        let pixels = vec![0xa5u8; 64 * 64 * 4];
        let image = Value::from((64i32, 64i32, 256i32, true, 8i32, 4i32, pixels));
        let mut nested = HashMap::new();
        nested.insert("deep", Value::from(vec!["a", "b"]));

        let mut map = HashMap::new();
        map.insert("image-data", image);
        map.insert("x-dict", Value::from(nested));
        map.insert("x-float", Value::F64(1.5));
        map.insert("x-variant", Value::Value(Box::new(Value::U8(2))));
        map.insert("sound-file", Value::from("/usr/share/sounds/x.oga"));
        map.insert("urgency", Value::U8(2));
        map.insert("category", Value::from("device.error"));
        let read = hints(&map);
        assert_eq!(read.urgency, Urgency::Critical);
        assert_eq!(read.category, "device.error");
    }

    #[test]
    fn a_hint_of_the_wrong_type_is_ignored_without_failing_the_call() {
        let mut map = HashMap::new();
        map.insert("urgency", Value::from(vec![1u8, 2, 3]));
        map.insert("transient", Value::from("yes"));
        map.insert("category", Value::U32(7));
        map.insert("desktop-entry", Value::Value(Box::new(Value::from("x"))));
        assert_eq!(hints(&map), Hints::default());
    }

    /// A deterministic stand-in for random bytes.
    fn noise(seed: u32) -> impl FnMut() -> u8 {
        let mut state = seed.wrapping_mul(2_654_435_761).max(1);
        move || {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            (state >> 8) as u8
        }
    }

    fn sample_hints() -> Vec<u8> {
        let mut nested = HashMap::new();
        nested.insert("deep", Value::from(vec!["a", "b"]));
        let mut map = HashMap::new();
        map.insert("urgency", Value::U8(2));
        map.insert("category", Value::from("im.received"));
        map.insert(
            "image-data",
            Value::from((2i32, 2i32, 8i32, true, vec![1u8; 16])),
        );
        map.insert("x-dict", Value::from(nested));
        to_bytes(Context::new_dbus(LE, 0), &map)
            .expect("the hints encode")
            .bytes()
            .to_vec()
    }

    fn sample_actions() -> Vec<u8> {
        let list: &[&str] = &["default", "Open", "reply", "Reply"];
        to_bytes(Context::new_dbus(LE, 0), &list)
            .expect("the actions encode")
            .bytes()
            .to_vec()
    }

    /// What was read, in a form that can be compared; `None` if refused.
    type Read = fn(&[u8]) -> Option<String>;

    fn read_hints(bytes: &[u8]) -> Option<String> {
        let data = Data::new(bytes, Context::new_dbus(LE, 0));
        let (read, _) = data.deserialize::<WireHints>().ok()?;
        Some(format!("{read:?}"))
    }

    fn read_actions(bytes: &[u8]) -> Option<String> {
        let data = Data::new(bytes, Context::new_dbus(LE, 0));
        let (read, _) = data.deserialize::<Actions<'_>>().ok()?;
        Some(format!("{read:?}"))
    }

    /// Input cut short is refused. The one exception is the very last
    /// byte when that is the terminator of a string, which the decoder
    /// does not insist on; what is read then is still the whole message.
    fn cut_short(whole: &[u8], read: Read) {
        let full = read(whole);
        assert!(full.is_some(), "the whole input was refused");
        for cut in 0..whole.len() {
            if let Some(partial) = read(&whole[..cut]) {
                assert_eq!(cut, whole.len() - 1, "cut at {cut} was accepted");
                assert_eq!(Some(partial), full);
            }
        }
    }

    #[test]
    fn hints_cut_short_are_refused() {
        cut_short(&sample_hints(), read_hints);
    }

    #[test]
    fn actions_cut_short_are_refused() {
        cut_short(&sample_actions(), read_actions);
    }

    #[test]
    fn damaged_input_is_refused_or_read_but_never_panics() {
        for (whole, read) in [
            (sample_hints(), read_hints as Read),
            (sample_actions(), read_actions as Read),
        ] {
            // Every byte in turn replaced by every kind of wrong value.
            for at in 0..whole.len() {
                for wrong in [0x00, 0x01, 0x7f, 0x80, 0xff, b'v', b'(', b'a'] {
                    let mut damaged = whole.clone();
                    damaged[at] = wrong;
                    read(&damaged);
                }
            }
            // And plain noise, of every length up to the real thing.
            for seed in 0..500 {
                let mut next = noise(seed);
                let length = seed as usize % (whole.len() + 1);
                let garbage: Vec<u8> = (0..length).map(|_| next()).collect();
                read(&garbage);
            }
        }
    }

    #[test]
    fn lengths_that_promise_more_than_is_there_are_refused() {
        // An array said to be four gigabytes long, with nothing in it.
        assert!(read_actions(&[0xff, 0xff, 0xff, 0xff]).is_none());
        assert!(read_hints(&[0xff, 0xff, 0xff, 0xff, 0, 0, 0, 0]).is_none());
        // A string inside the array that runs past the end.
        assert!(read_actions(&[8, 0, 0, 0, 0xff, 0, 0, 0, b'a', 0, 0, 0]).is_none());
        // Text that is not UTF-8.
        assert!(read_actions(&[8, 0, 0, 0, 2, 0, 0, 0, 0xc3, 0x28, 0, 0]).is_none());
    }

    #[test]
    fn actions_are_read_in_order() {
        assert_eq!(
            actions(&["default", "Open", "reply", "Reply"]),
            ["default", "Open", "reply", "Reply"]
        );
        assert!(actions(&[]).is_empty());
    }

    #[test]
    fn only_the_first_actions_are_kept() {
        let many: Vec<String> = (0..10_000).map(|n| format!("action {n}")).collect();
        let many: Vec<&str> = many.iter().map(String::as_str).collect();
        let kept = actions(&many);
        assert_eq!(kept.len(), SCAN_MAX * 2);
        assert_eq!(kept[0], "action 0");
        assert_eq!(
            kept[SCAN_MAX * 2 - 1],
            format!("action {}", SCAN_MAX * 2 - 1)
        );
    }
}
