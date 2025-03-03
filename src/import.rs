use crate::import::elgato::{Action, ActionBehavior, PageManifest, ProfileManifest, ProfileManifestPages};
use base32::Alphabet;
use clap::Args;
use eyre::{Context, OptionExt, ensure};
use regex::Regex;
use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek};
use std::path::PathBuf;
use std::sync::{Arc, LazyLock, OnceLock};
use tracing::{debug, info};
use uuid::Uuid;
use zip::ZipArchive;
use crate::config;
use crate::config::Config;

#[derive(Debug, Eq, PartialEq, Args, Clone)]
pub struct ImportArgs {
    pub path: PathBuf,
    #[arg(long, required = true)]
    pub profile_name: String,
}

#[tracing::instrument(skip(args))]
pub(crate) async fn run(args: &ImportArgs) -> eyre::Result<()> {
    let args = args.clone();
    _ = tokio::task::spawn_blocking(move || run_sync(args)).await?;
    Ok(())
}

pub(crate) fn run_sync(args: ImportArgs) -> eyre::Result<Config> {
    info!("Running imports with args: {:#?}", args);
    let file = File::open(&args.path)
        .with_context(|| format!("Failed to import file {:?}", &args.path))?;
    let mut archive = ZipArchive::new(file)
        .with_context(|| format!("Failed to open zip archive {:?}", &args.path))?;

    let mut manifest_paths = parse_manifest_paths(&mut archive)?;

    let selected_profile = find_selected_profile(args, &mut archive, &mut manifest_paths)?;
    info!(
        "Selected profile: {:?} ({} manifests)",
        selected_profile,
        manifest_paths.len()
    );

    let profiles = decode_uuids(manifest_paths)?;
    
    // parse manifests
    let mut profile_manifests = HashMap::new();
    for page in profiles.values() {
        let manifest_file = archive.by_name(&page.manifest_path).with_context(|| {
            format!("Failed to read page manifest file {}", &page.manifest_path)
        })?;
        let manifest: PageManifest = serde_json::from_reader(manifest_file).with_context(|| {
            format!("Failed to parse page manifest file {}", &page.manifest_path)
        })?;
        profile_manifests.insert(page.profile_id, manifest);
    }
    
    // reverse map profile names
    let mut profile_names = HashMap::new();
    for manifest in profile_manifests.values() {
        let Some(keypad) = manifest
            .controllers
            .iter()
            .filter(|c| c.ty == "Keypad")
            .next()
        else {
            continue;
        };
        for (_, action) in keypad.actions.iter() {
            if let ActionBehavior::OpenChild { settings } = &action.behavior {
                if let Some(title) = action.states.get(action.state).and_then(|x| x.title.as_ref()) {
                    profile_names.insert(settings.profile_uuid, &title[..]);
                }
            }
        }
    }
    
    // generate config
    let mut config_pages = HashMap::new();
    for (id, manifest) in profile_manifests.iter() {
        let mut buttons = Vec::new();
        let Some(keypad) = manifest
            .controllers
            .iter()
            .filter(|c| c.ty == "Keypad")
            .next()
        else {
            continue;
        };
        for (pos, action) in keypad.actions.iter() {
            match &action.behavior {
                ActionBehavior::BackToParent => {}
                ActionBehavior::PlayAudio { settings } => {
                    buttons.push(config::Button{
                        label: label_of(action),
                        behavior: config::ButtonBehavior::PlaySound {
                            path: settings.path.clone()
                        }
                    });
                }
                ActionBehavior::OpenChild { settings } => {
                    buttons.push(config::Button{
                        label: label_of(action),
                        behavior: config::ButtonBehavior::PushPage(settings.profile_uuid)
                    })
                }
                ActionBehavior::Unknown => {
                    debug!("Unknown action behavior: {}{:?}{:?}", id, pos, action);
                }
            }
        }
        config_pages.insert(*id, Arc::new(config::Page{
            name: profile_names.get(&id).unwrap_or(&"Page?").to_string(),
            buttons
        }));
    }
    
    let c = config::Config{
        pages: config_pages,
        start_page: selected_profile.current,
    };

    Ok(c)
}

fn label_of(action: &Action) -> Arc<String> {
    static EMPTY_STRING: LazyLock<Arc<String>> = LazyLock::new(|| Arc::new("".to_string()));
    action.states.get(action.state)
        .and_then(|x| x.title.clone())
        .unwrap_or_else(|| EMPTY_STRING.clone())
}

struct PageEntry {
    profile_id: Uuid,
    manifest_path: String,
}

fn decode_uuids(
    manifest_paths: Vec<(String, String, Option<String>)>,
) -> eyre::Result<HashMap<Uuid, PageEntry>> {
    let mut profiles = HashMap::new();
    for (name, _, inner_profile) in manifest_paths {
        let Some(mut inner_profile) = inner_profile else {
            continue;
        };
        decode_uuid(&mut profiles, name, &mut inner_profile)?;
    }
    return Ok(profiles);
}

#[tracing::instrument(skip(profiles,name), level = "trace")]
fn decode_uuid(profiles: &mut HashMap<Uuid, PageEntry>, name: String, mut inner_profile: &mut String) -> eyre::Result<()> {
    ensure!(inner_profile.len() > 0);
    ensure!(inner_profile.ends_with('Z'));

    // I'm not 100% sure what the "real" encoding of the profile directory names are, but
    // someone else reverse-engineered the following procedure:
    // https://github.com/data-enabler/streamdeck-profile-generator/blob/master/lib/ids.js
    // remove hyphens and pad (so that the length is a multiple of 5)
    // convert to base32
    // replace V with W
    // replace U with V
    // add Z at the end
    // code below performs this transformation in reverse

    inner_profile.pop();
    replace_ascii(&mut inner_profile, b'V', b'U');
    replace_ascii(&mut inner_profile, b'W', b'V');
    inner_profile.make_ascii_uppercase();
    let decoded_bytes = base32::decode(Alphabet::Rfc4648Hex { padding: false }, &inner_profile)
        .ok_or_eyre("failed to decode profile directory name")?;
    let inner_id = Uuid::from_slice(&decoded_bytes[..])
        .with_context(|| format!("{} is not a valid UUID", &inner_profile))?;
    profiles.insert(
        inner_id,
        PageEntry {
            profile_id: inner_id,
            manifest_path: name,
        },
    );
    return Ok(());
    fn replace_ascii(s: &mut str, search: u8, replace: u8) {
        assert!(search < 128);
        assert!(replace < 128);
        // Safety: both the search and replace values are ASCII and thus valid UTF-8 and cannot
        // occur in the middle of a multibyte character.
        unsafe {
            for c in s.as_bytes_mut() {
                if *c == search {
                    *c = replace;
                }
            }
        }
    }
}

fn parse_manifest_paths<R>(
    archive: &mut ZipArchive<R>,
) -> eyre::Result<Vec<(String, String, Option<String>)>>
where
    R: Read + Seek,
{
    static MANIFEST_PATTERN: OnceLock<Regex> = OnceLock::new();
    let manifest_pattern = MANIFEST_PATTERN.get_or_init(|| {
        Regex::new(r"^([A-Z0-9-]+).sdProfile/(?:Profiles/([A-Z0-9]+)/)?manifest\.json$")
            .expect("Regular expression to be valid")
    });

    let manifest_paths = archive
        .file_names()
        .filter_map(|name| {
            manifest_pattern.captures(name).map(|captures| {
                let top_profile = captures
                    .get(1)
                    .expect("capture group 1 should always exist");
                let inner_profile = captures.get(2);
                (
                    name.to_owned(),
                    top_profile.as_str().to_owned(),
                    inner_profile.map(|m| m.as_str().to_owned()),
                )
            })
        })
        .inspect(|(name, top_profile, inner_profile)| {
            debug!(
                "Found manifest in archive: {}/{:?}: {}",
                top_profile, inner_profile, name
            )
        })
        .collect::<Vec<_>>();

    Ok(manifest_paths)
}

fn find_selected_profile(
    args: ImportArgs,
    archive: &mut ZipArchive<File>,
    manifest_paths: &mut Vec<(String, String, Option<String>)>,
) -> eyre::Result<ProfileManifestPages> {
    // search top-level manifests for the configured profile
    let mut selected_profile = None;
    let mut selected_profile_id = None;
    let stripped_arg_profile_name = args.profile_name.trim_matches('"');
    for (name, top_profile, inner_profile) in manifest_paths.iter() {
        if inner_profile.is_some() {
            continue;
        }
        let mut manifest_file = archive
            .by_name(&name)
            .with_context(|| format!("Failed to open manifest file {:?}", name))?;
        let mut manifest_buf = Vec::new();
        manifest_file.read_to_end(&mut manifest_buf)?;
        let manifest_buf = String::from_utf8(manifest_buf)?;
        let manifest: ProfileManifest = serde_json::from_str(&manifest_buf)?;
        if manifest.name == args.profile_name || manifest.name == stripped_arg_profile_name {
            info!(
                "Found profile manifest: {}/{}",
                top_profile, args.profile_name
            );
            selected_profile = Some(manifest);
            selected_profile_id = Some(top_profile.clone());
            break;
        }
    }
    let selected_profile = selected_profile.ok_or_eyre("Profile not found in archive")?;
    let selected_profile_id = selected_profile_id.ok_or_eyre("Profile not found in archive")?;

    // throw away all manifests that aren't children of the selected profile
    manifest_paths.retain_mut(|(_, top_profile, inner_profile)| {
        inner_profile.is_some() && &selected_profile_id == top_profile
    });
    Ok(selected_profile.pages)
}

mod elgato;
