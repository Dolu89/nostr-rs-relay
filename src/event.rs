//! Event parsing and validation
use crate::config;
use crate::error::Error::*;
use crate::error::Result;
use bitcoin_hashes::{sha256, Hash};
use log::*;
use secp256k1::{schnorrsig, Secp256k1};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::value::Value;
use serde_json::Number;
use std::str::FromStr;
use std::time::SystemTime;

/// Event command in network format
#[derive(Serialize, Deserialize, PartialEq, Debug, Clone)]
pub struct EventCmd {
    cmd: String, // expecting static "EVENT"
    event: Event,
}

/// Event parsed
#[derive(Serialize, Deserialize, PartialEq, Debug, Clone)]
pub struct Event {
    pub id: String,
    pub(crate) pubkey: String,
    pub(crate) created_at: u64,
    pub(crate) kind: u64,
    #[serde(deserialize_with = "tag_from_string")]
    // NOTE: array-of-arrays may need to be more general than a string container
    pub(crate) tags: Vec<Vec<String>>,
    pub(crate) content: String,
    pub(crate) sig: String,
}

/// Simple tag type for array of array of strings.
type Tag = Vec<Vec<String>>;

/// Deserializer that ensures we always have a [`Tag`].
fn tag_from_string<'de, D>(deserializer: D) -> Result<Tag, D::Error>
where
    D: Deserializer<'de>,
{
    let opt = Option::deserialize(deserializer)?;
    Ok(opt.unwrap_or_else(Vec::new))
}

/// Convert network event to parsed/validated event.
impl From<EventCmd> for Result<Event> {
    fn from(ec: EventCmd) -> Result<Event> {
        // ensure command is correct
        if ec.cmd != "EVENT" {
            Err(CommandUnknownError)
        } else if ec.event.is_valid() {
            Ok(ec.event)
        } else {
            Err(EventInvalid)
        }
    }
}

/// Seconds since 1970
fn unix_time() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|x| x.as_secs())
        .unwrap_or(0)
}

impl Event {
    /// Create a short event identifier, suitable for logging.
    pub fn get_event_id_prefix(&self) -> String {
        self.id.chars().take(8).collect()
    }

    /// Check if this event has a valid signature.
    fn is_valid(&self) -> bool {
        // TODO: return a Result with a reason for invalid events
        // don't bother to validate an event with a timestamp in the distant future.
        let config = config::SETTINGS.read().unwrap();
        let max_future_sec = config.options.reject_future_seconds;
        if let Some(allowable_future) = max_future_sec {
            let curr_time = unix_time();
            // calculate difference, plus how far future we allow
            if curr_time + (allowable_future as u64) < self.created_at {
                let delta = self.created_at - curr_time;
                debug!(
                    "Event is too far in the future ({} seconds), rejecting",
                    delta
                );
                return false;
            }
        }
        // validation is performed by:
        // * parsing JSON string into event fields
        // * create an array:
        // ** [0, pubkey-hex-string, created-at-num, kind-num, tags-array-of-arrays, content-string]
        // * serialize with no spaces/newlines
        let c_opt = self.to_canonical();
        if c_opt.is_none() {
            info!("event could not be canonicalized");
            return false;
        }
        let c = c_opt.unwrap();
        // * compute the sha256sum.
        let digest: sha256::Hash = sha256::Hash::hash(c.as_bytes());
        let hex_digest = format!("{:x}", digest);
        // * ensure the id matches the computed sha256sum.
        if self.id != hex_digest {
            return false;
        }
        // * validate the message digest (sig) using the pubkey & computed sha256 message hash.
        let secp = Secp256k1::new();
        let sig = schnorrsig::Signature::from_str(&self.sig).unwrap();
        let message = secp256k1::Message::from(digest);
        let pubkey = schnorrsig::PublicKey::from_str(&self.pubkey).unwrap();
        let verify = secp.schnorrsig_verify(&sig, &message, &pubkey);
        matches!(verify, Ok(()))
    }

    /// Convert event to canonical representation for signing.
    fn to_canonical(&self) -> Option<String> {
        // create a JsonValue for each event element
        let mut c: Vec<Value> = vec![];
        // id must be set to 0
        let id = Number::from(0_u64);
        c.push(serde_json::Value::Number(id));
        // public key
        c.push(Value::String(self.pubkey.to_owned()));
        // creation time
        let created_at = Number::from(self.created_at);
        c.push(serde_json::Value::Number(created_at));
        // kind
        let kind = Number::from(self.kind);
        c.push(serde_json::Value::Number(kind));
        // tags
        c.push(self.tags_to_canonical());
        // content
        c.push(Value::String(self.content.to_owned()));
        serde_json::to_string(&Value::Array(c)).ok()
    }

    /// Convert tags to a canonical form for signing.
    fn tags_to_canonical(&self) -> Value {
        let mut tags = Vec::<Value>::new();
        // iterate over self tags,
        for t in self.tags.iter() {
            // each tag is a vec of strings
            let mut a = Vec::<Value>::new();
            for v in t.iter() {
                a.push(serde_json::Value::String(v.to_owned()));
            }
            tags.push(serde_json::Value::Array(a));
        }
        serde_json::Value::Array(tags)
    }

    /// Get a list of event tags.
    pub fn get_event_tags(&self) -> Vec<&str> {
        let mut etags = vec![];
        for t in self.tags.iter() {
            if t.len() >= 2 && t.get(0).unwrap() == "e" {
                etags.push(&t.get(1).unwrap()[..]);
            }
        }
        etags
    }

    /// Get a list of pubkey/petname tags.
    pub fn get_pubkey_tags(&self) -> Vec<&str> {
        let mut ptags = vec![];
        for t in self.tags.iter() {
            if t.len() >= 2 && t.get(0).unwrap() == "p" {
                ptags.push(&t.get(1).unwrap()[..]);
            }
        }
        ptags
    }

    /// Check if a given event is referenced in an event tag.
    pub fn event_tag_match(&self, eventid: &str) -> bool {
        self.get_event_tags().contains(&eventid)
    }

    /// Check if a given event is referenced in an event tag.
    pub fn pubkey_tag_match(&self, pubkey: &str) -> bool {
        self.get_pubkey_tags().contains(&pubkey)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn simple_event() -> Event {
        Event {
            id: "0".to_owned(),
            pubkey: "0".to_owned(),
            created_at: 0,
            kind: 0,
            tags: vec![],
            content: "".to_owned(),
            sig: "0".to_owned(),
        }
    }

    #[test]
    fn event_creation() {
        // create an event
        let event = simple_event();
        assert_eq!(event.id, "0");
    }

    #[test]
    fn event_serialize() -> Result<()> {
        // serialize an event to JSON string
        let event = simple_event();
        let j = serde_json::to_string(&event)?;
        assert_eq!(j, "{\"id\":\"0\",\"pubkey\":\"0\",\"created_at\":0,\"kind\":0,\"tags\":[],\"content\":\"\",\"sig\":\"0\"}");
        Ok(())
    }

    #[test]
    fn empty_event_tag_match() -> Result<()> {
        let event = simple_event();
        assert!(!event.event_tag_match("foo"));
        Ok(())
    }

    #[test]
    fn single_event_tag_match() -> Result<()> {
        let mut event = simple_event();
        event.tags = vec![vec!["e".to_owned(), "foo".to_owned()]];
        assert!(event.event_tag_match("foo"));
        Ok(())
    }

    #[test]
    fn event_tags_serialize() -> Result<()> {
        // serialize an event with tags to JSON string
        let mut event = simple_event();
        event.tags = vec![
            vec![
                "e".to_owned(),
                "xxxx".to_owned(),
                "wss://example.com".to_owned(),
            ],
            vec![
                "p".to_owned(),
                "yyyyy".to_owned(),
                "wss://example.com:3033".to_owned(),
            ],
        ];
        let j = serde_json::to_string(&event)?;
        assert_eq!(j, "{\"id\":\"0\",\"pubkey\":\"0\",\"created_at\":0,\"kind\":0,\"tags\":[[\"e\",\"xxxx\",\"wss://example.com\"],[\"p\",\"yyyyy\",\"wss://example.com:3033\"]],\"content\":\"\",\"sig\":\"0\"}");
        Ok(())
    }

    #[test]
    fn event_deserialize() -> Result<()> {
        let raw_json = r#"{"id":"1384757da583e6129ce831c3d7afc775a33a090578f888dd0d010328ad047d0c","pubkey":"bbbd9711d357df4f4e498841fd796535c95c8e751fa35355008a911c41265fca","created_at":1612650459,"kind":1,"tags":null,"content":"hello world","sig":"59d0cc47ab566e81f72fe5f430bcfb9b3c688cb0093d1e6daa49201c00d28ecc3651468b7938642869ed98c0f1b262998e49a05a6ed056c0d92b193f4e93bc21"}"#;
        let e: Event = serde_json::from_str(raw_json)?;
        assert_eq!(e.kind, 1);
        assert_eq!(e.tags.len(), 0);
        Ok(())
    }

    #[test]
    fn event_canonical() {
        let e = Event {
            id: "999".to_owned(),
            pubkey: "012345".to_owned(),
            created_at: 501234,
            kind: 1,
            tags: vec![],
            content: "this is a test".to_owned(),
            sig: "abcde".to_owned(),
        };
        let c = e.to_canonical();
        let expected = Some(r#"[0,"012345",501234,1,[],"this is a test"]"#.to_owned());
        assert_eq!(c, expected);
    }

    #[test]
    fn event_canonical_with_tags() {
        let e = Event {
            id: "999".to_owned(),
            pubkey: "012345".to_owned(),
            created_at: 501234,
            kind: 1,
            tags: vec![
                vec!["#e".to_owned(), "aoeu".to_owned()],
                vec![
                    "#p".to_owned(),
                    "aaaa".to_owned(),
                    "ws://example.com".to_owned(),
                ],
            ],
            content: "this is a test".to_owned(),
            sig: "abcde".to_owned(),
        };
        let c = e.to_canonical();
        let expected_json = r###"[0,"012345",501234,1,[["#e","aoeu"],["#p","aaaa","ws://example.com"]],"this is a test"]"###;
        let expected = Some(expected_json.to_owned());
        assert_eq!(c, expected);
    }
}
