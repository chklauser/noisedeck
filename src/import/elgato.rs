use serde::{Deserialize, Deserializer};
use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;
use uuid::Uuid;

#[derive(Deserialize, Debug)]
#[serde(rename_all = "PascalCase")]
pub struct ProfileManifest {
    pub name: String,
    pub pages: ProfileManifestPages,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "PascalCase")]
pub struct ProfileManifestPages {
    pub current: Uuid,
    pub default: Uuid,
    pub pages: Vec<Uuid>,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "PascalCase")]
pub struct PageManifest {
    pub controllers: Vec<Controller>,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "PascalCase")]
pub struct Controller {
    #[serde(rename = "Type")]
    pub ty: String,
    pub actions: HashMap<Pos, Action>,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "PascalCase")]
pub struct Action {
    pub state: usize,
    pub states: Vec<State>,
    #[serde(flatten)]
    pub behavior: ActionBehavior,
}

#[derive(Deserialize, Debug, Default)]
#[serde(tag = "UUID")]
pub enum ActionBehavior {
    #[serde(rename = "com.elgato.streamdeck.profile.backtoparent")]
    BackToParent,

    #[serde(rename = "com.elgato.streamdeck.soundboard.playaudio")]
    PlayAudio {
        #[serde(rename = "Settings")]
        settings: AudioSettings,
    },

    #[serde(rename = "com.elgato.streamdeck.profile.openchild")]
    OpenChild {
        #[serde(rename = "Settings")]
        settings: OpenChildSettings,
    },

    #[default]
    #[serde(other)]
    Unknown,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "PascalCase")]
pub struct OpenChildSettings {
    #[serde(rename = "ProfileUUID")]
    pub profile_uuid: Uuid,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct AudioSettings {
    pub fade_length: Option<u32>,
    pub volume: u8,
    pub path: Arc<String>,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "PascalCase")]
pub struct State {
    #[serde(default)]
    pub show_title: bool,
    pub title: Option<Arc<String>>,
}

#[derive(Debug, Eq, PartialEq, Hash)]
pub struct Pos(u8, u8);
impl FromStr for Pos {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let parts: Vec<&str> = s.split(',').collect();
        if parts.len() != 2 {
            return Err("Invalid coordinate format".to_string());
        }
        let x = parts[0]
            .parse()
            .map_err(|_| "Invalid x coordinate".to_string())?;
        let y = parts[1]
            .parse()
            .map_err(|_| "Invalid y coordinate".to_string())?;
        Ok(Pos(x, y))
    }
}
impl<'de> Deserialize<'de> for Pos {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}
