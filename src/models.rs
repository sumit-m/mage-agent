//! Buzz's Model, Aspect ratio and Resolution menus, read from mage's
//! `models.ini` (next to mage.exe), and the architecture each model needs.
//!
//! Each model's aspects, resolutions, defaults and pixel sizes come from its
//! section; Buzz offers an aspect or resolution only for the models that list
//! it (`when`), shows the size for the model and resolution picked (`sizes`),
//! and falls back to the model's own defaults (`defaults`) when a pick isn't
//! offered after a model switch. Resolutions are offered upper-case: mage
//! accepts either case and sends each model the case it needs.

use std::path::Path;

use serde_json::{json, Map, Value};

use crate::presets;

const MAX_OPTIONS: usize = 50;
const MAX_TEXT: usize = 64;
const DEFAULT_MODEL: &str = "mango-v4";

#[derive(Debug, PartialEq)]
pub struct Model {
    pub id: String,
    pub architecture: String,
    display: String,
    aspects: Vec<String>,
    aspect_default: Option<String>,
    /// Upper-case tokens, in the file's order.
    resolutions: Vec<String>,
    res_default: Option<String>,
    /// `(resolution, size per aspect)`, sizes in `aspects` order.
    sizes: Vec<(String, Vec<Option<[u32; 2]>>)>,
    speed: Option<String>,
}

impl Model {
    fn size(&self, aspect: &str, res: &str) -> Option<[u32; 2]> {
        let index = self.aspects.iter().position(|a| a == aspect)?;
        self.sizes
            .iter()
            .find(|(r, _)| r == res)
            .and_then(|(_, sizes)| sizes.get(index).copied().flatten())
    }
}

fn list(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(str::to_string)
        .collect()
}

fn size(value: &str) -> Option<[u32; 2]> {
    let (width, height) = value.trim().split_once(['x', 'X'])?;
    Some([width.trim().parse().ok()?, height.trim().parse().ok()?])
}

/// The image models in `models.ini`, in file order. A section without an
/// architecture, aspects or resolutions, or with a name Buzz can't send, is
/// left out.
pub fn parse(text: &str) -> Vec<Model> {
    presets::parse(text)
        .into_iter()
        .filter_map(|(id, fields)| {
            let get = |key: &str| {
                fields
                    .iter()
                    .find(|(k, v)| k.eq_ignore_ascii_case(key) && !v.is_empty())
                    .map(|(_, v)| v.as_str())
            };
            if get("media").is_some_and(|media| media != "image") {
                return None;
            }
            let architecture = get("architecture")?.to_string();
            let aspects: Vec<String> = list(get("aspects")?)
                .into_iter()
                .filter(|a| presets::usable_name(a))
                .collect();
            let resolutions: Vec<String> = list(get("resolutions")?)
                .into_iter()
                .filter(|r| presets::usable_name(r))
                .map(|r| r.to_ascii_uppercase())
                .collect();
            if !presets::usable_name(&id) || aspects.is_empty() || resolutions.is_empty() {
                eprintln!("mage-agent: model {id:?} left out of the panel");
                return None;
            }
            let sizes = resolutions
                .iter()
                .map(|res| {
                    let line = get(&format!("aspect_size_{res}"))
                        .map(list)
                        .unwrap_or_default();
                    let sizes = (0..aspects.len())
                        .map(|i| line.get(i).and_then(|s| size(s)))
                        .collect();
                    (res.clone(), sizes)
                })
                .collect();
            let display = get("display")
                .filter(|d| d.chars().count() <= MAX_TEXT)
                .unwrap_or(&id)
                .to_string();
            Some(Model {
                aspect_default: get("aspect_default")
                    .filter(|a| aspects.iter().any(|x| x == a))
                    .map(str::to_string),
                res_default: get("res_default")
                    .map(str::to_ascii_uppercase)
                    .filter(|r| resolutions.contains(r)),
                speed: get("speed").map(capitalized),
                id,
                architecture,
                display,
                aspects,
                resolutions,
                sizes,
            })
        })
        .take(MAX_OPTIONS)
        .collect()
}

fn capitalized(word: &str) -> String {
    let mut chars = word.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_uppercase().chain(chars).collect()
    })
}

/// Values in first-seen order across `models`.
fn union<'a>(lists: impl Iterator<Item = &'a Vec<String>>) -> Vec<String> {
    let mut all: Vec<String> = Vec::new();
    for item in lists.flatten() {
        if !all.contains(item) {
            all.push(item.clone());
        }
    }
    all
}

/// `{"model": [ids]}`, or nothing when every model in `of` qualifies.
fn when_models(ids: &[&str], of: &[&str]) -> Option<Value> {
    (ids.len() < of.len()).then(|| json!({ "model": ids }))
}

fn auto() -> Value {
    json!({ "value": "auto", "label": "Auto", "modes": ["edit"] })
}

/// The Model, Aspect ratio and Resolution controls, or `None` without models.
pub fn controls(models: &[Model]) -> Option<[Value; 3]> {
    let default = models
        .iter()
        .find(|m| m.id == DEFAULT_MODEL)
        .or_else(|| models.first())?;
    let all: Vec<&str> = models.iter().map(|m| m.id.as_str()).collect();

    let model_options: Vec<Value> = models
        .iter()
        .map(|model| {
            let mut option = json!({ "value": model.id, "label": model.display });
            if let Some(speed) = &model.speed {
                option["hint"] = speed.clone().into();
            }
            let mut defaults = Map::new();
            if let Some(aspect) = &model.aspect_default {
                defaults.insert("aspect".into(), aspect.clone().into());
            }
            if let Some(res) = &model.res_default {
                defaults.insert("res".into(), res.clone().into());
            }
            if !defaults.is_empty() {
                option["defaults"] = defaults.into();
            }
            option
        })
        .collect();

    let mut resolutions = union(models.iter().map(|m| &m.resolutions));
    resolutions.sort_by_key(|res| {
        res.trim_end_matches(|c: char| !c.is_ascii_digit())
            .parse::<u32>()
            .unwrap_or(u32::MAX)
    });

    let aspect_options = std::iter::once(auto()).chain(
        union(models.iter().map(|m| &m.aspects))
            .into_iter()
            .take(MAX_OPTIONS - 1)
            .map(|aspect| {
                let with_aspect: Vec<&Model> = models
                    .iter()
                    .filter(|m| m.aspects.contains(&aspect))
                    .collect();
                let ids: Vec<&str> = with_aspect.iter().map(|m| m.id.as_str()).collect();
                let mut option = json!({ "value": aspect, "label": capitalized(&aspect) });
                let base = std::iter::once(default)
                    .chain(with_aspect.iter().copied())
                    .find_map(|m| {
                        let res = m.res_default.as_ref().unwrap_or(&m.resolutions[0]);
                        m.size(&aspect, res)
                    });
                if let Some(base) = base {
                    option["size"] = json!(base);
                }
                if let Some(when) = when_models(&ids, &all) {
                    option["when"] = when;
                }
                let mut sizes = Vec::new();
                for res in &resolutions {
                    let offering: Vec<&Model> = with_aspect
                        .iter()
                        .copied()
                        .filter(|m| m.resolutions.contains(res))
                        .collect();
                    let offering_ids: Vec<&str> = offering.iter().map(|m| m.id.as_str()).collect();
                    let mut groups: Vec<([u32; 2], Vec<&str>)> = Vec::new();
                    for model in &offering {
                        if let Some(size) = model.size(&aspect, res) {
                            match groups.iter_mut().find(|(s, _)| *s == size) {
                                Some((_, ids)) => ids.push(&model.id),
                                None => groups.push((size, vec![&model.id])),
                            }
                        }
                    }
                    for (size, ids) in groups {
                        let mut when = json!({ "res": [res] });
                        if let Some(Value::Object(models)) = when_models(&ids, &offering_ids) {
                            when.as_object_mut().unwrap().extend(models);
                        }
                        sizes.push(json!({ "when": when, "size": size }));
                    }
                }
                if !sizes.is_empty() {
                    option["sizes"] = sizes.into();
                }
                option
            }),
    );

    let res_options =
        std::iter::once(auto()).chain(resolutions.iter().take(MAX_OPTIONS - 1).map(|res| {
            let ids: Vec<&str> = models
                .iter()
                .filter(|m| m.resolutions.contains(res))
                .map(|m| m.id.as_str())
                .collect();
            let mut option = json!({ "value": res, "label": res });
            if let Some(when) = when_models(&ids, &all) {
                option["when"] = when;
            }
            option
        }));

    Some([
        json!({
            "id": "model", "kind": "list", "label": "Model", "icon": "sparkles",
            "default": default.id, "sidecarKey": "model_id", "options": model_options
        }),
        json!({
            "id": "aspect", "kind": "shapes", "label": "Aspect ratio",
            "default": default.aspect_default.as_ref().unwrap_or(&default.aspects[0]),
            "defaultByMode": { "edit": "auto" }, "sidecarKey": "aspect_ratio",
            "options": aspect_options.collect::<Vec<_>>()
        }),
        json!({
            "id": "res", "kind": "segmented", "label": "Resolution",
            "default": default.res_default.as_ref().unwrap_or(&default.resolutions[0]),
            "defaultByMode": { "edit": "auto" }, "sidecarKey": "resolution",
            "options": res_options.collect::<Vec<_>>()
        }),
    ])
}

/// Puts each control in place of the one with its id, or, when missing, at the
/// front in this order. Returns whether anything changed.
fn apply(controls: &mut Vec<Value>, new: [Value; 3]) -> bool {
    let mut changed = false;
    let mut at = 0;
    for control in new {
        match controls.iter().position(|c| c["id"] == control["id"]) {
            Some(i) => {
                if controls[i] != control {
                    controls[i] = control;
                    changed = true;
                }
                at = i + 1;
            }
            None => {
                controls.insert(at, control);
                at += 1;
                changed = true;
            }
        }
    }
    changed
}

fn read(models_ini: &Path) -> Result<Vec<Model>, String> {
    match std::fs::read(models_ini) {
        Ok(bytes) => Ok(parse(&presets::decode(&bytes))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(format!("{}: {e}", models_ini.display())),
    }
}

/// Rewrites the harness file's model, aspect and resolution controls from
/// `models_ini`. Without models the controls are left as they are. Returns
/// whether the file changed.
pub fn refresh(harness_file: &Path, models_ini: &Path) -> Result<bool, String> {
    let Some(new) = controls(&read(models_ini)?) else {
        return Ok(false);
    };
    presets::update_controls(harness_file, |controls| apply(controls, new))
}

/// The architecture `models_ini` gives `model`, which mage needs as `--arch`.
pub fn architecture(models_ini: &Path, model: &str) -> Option<String> {
    read(models_ini)
        .ok()?
        .into_iter()
        .find(|m| m.id.eq_ignore_ascii_case(model))
        .map(|m| m.architecture)
}

#[cfg(test)]
mod tests {
    use super::*;

    const INI: &str = "; catalog\n\
        [mango-v4]\n\
        architecture=mango\n\
        display=Mango 4\n\
        media=image\n\
        aspects=portrait,square\n\
        aspect_default=portrait\n\
        resolutions=1K,2K,4K\n\
        res_default=1K\n\
        aspect_size_1K=896x1152,1024x1024\n\
        aspect_size_2K=1792x2304,2048x2048\n\
        aspect_size_4K=3584x4608,4096x4096\n\
        speed=slow\n\
        [flux2-dev]\n\
        architecture=flux2\n\
        display=FLUX 2 Dev\n\
        aspects=portrait,square,cinema\n\
        aspect_default=cinema\n\
        resolutions=1k,2k\n\
        res_default=1k\n\
        aspect_size_1k=896x1152,1024x1024,1344x768\n\
        aspect_size_2k=1280x1616,1440x1440,1920x1072\n\
        speed=slow\n\
        [clip]\n\
        architecture=vid\n\
        media=video\n\
        aspects=square\n\
        resolutions=1K\n\
        [broken]\n\
        display=No architecture\n";

    #[test]
    fn parse_keeps_image_models_with_what_they_need() {
        let models = parse(INI);
        let ids: Vec<&str> = models.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, ["mango-v4", "flux2-dev"]);
        let flux = &models[1];
        assert_eq!(flux.resolutions, ["1K", "2K"]);
        assert_eq!(flux.res_default.as_deref(), Some("1K"));
        assert_eq!(flux.size("cinema", "2K"), Some([1920, 1072]));
        assert_eq!(models[0].size("cinema", "1K"), None);
        assert_eq!(models[0].speed.as_deref(), Some("Slow"));
    }

    #[test]
    fn controls_follow_each_model() {
        let [model, aspect, res] = controls(&parse(INI)).unwrap();
        assert_eq!(model["default"], "mango-v4");
        assert_eq!(
            model["options"][1],
            json!({ "value": "flux2-dev", "label": "FLUX 2 Dev", "hint": "Slow",
                    "defaults": { "aspect": "cinema", "res": "1K" } })
        );

        let options = res["options"].as_array().unwrap();
        let values: Vec<&str> = options
            .iter()
            .map(|o| o["value"].as_str().unwrap())
            .collect();
        assert_eq!(values, ["auto", "1K", "2K", "4K"]);
        assert!(options[1].get("when").is_none());
        assert_eq!(options[3]["when"], json!({ "model": ["mango-v4"] }));

        let portrait = &aspect["options"][1];
        assert_eq!(portrait["size"], json!([896, 1152]));
        assert!(portrait.get("when").is_none());
        assert_eq!(
            portrait["sizes"],
            json!([
                { "when": { "res": ["1K"] }, "size": [896, 1152] },
                { "when": { "res": ["2K"], "model": ["mango-v4"] }, "size": [1792, 2304] },
                { "when": { "res": ["2K"], "model": ["flux2-dev"] }, "size": [1280, 1616] },
                { "when": { "res": ["4K"] }, "size": [3584, 4608] }
            ])
        );
        let cinema = &aspect["options"][3];
        assert_eq!(cinema["when"], json!({ "model": ["flux2-dev"] }));
        assert_eq!(cinema["size"], json!([1344, 768]));
        assert_eq!(aspect["options"][0]["value"], "auto");
    }

    #[test]
    fn refresh_replaces_or_inserts_the_three_controls() {
        let dir = std::env::temp_dir().join(format!("mage-models-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("mage.json");
        let ini = dir.join("models.ini");
        let read_ids = || {
            let harness: Value =
                serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
            harness["composer"]["controls"]
                .as_array()
                .unwrap()
                .iter()
                .map(|c| c["id"].as_str().unwrap().to_string())
                .collect::<Vec<_>>()
        };
        let harness = json!({ "composer": { "controls": [
            { "id": "preset", "kind": "preset" }, { "id": "repo", "kind": "repoFolder" }
        ] } });
        std::fs::write(&file, harness.to_string()).unwrap();

        assert!(!refresh(&file, &ini).unwrap());
        std::fs::write(&ini, INI).unwrap();
        assert!(refresh(&file, &ini).unwrap());
        assert_eq!(read_ids(), ["model", "aspect", "res", "preset", "repo"]);
        assert!(!refresh(&file, &ini).unwrap());
        assert_eq!(architecture(&ini, "FLUX2-dev").as_deref(), Some("flux2"));
        assert_eq!(architecture(&ini, "clip"), None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
