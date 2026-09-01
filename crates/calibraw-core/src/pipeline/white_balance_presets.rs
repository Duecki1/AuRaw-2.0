use serde::Deserialize;
use std::collections::HashMap;
use std::sync::OnceLock;

#[derive(Clone, Debug)]
pub struct WhiteBalancePreset {
    pub name: String,
    pub coefficients: [f32; 4],
}

#[derive(Deserialize)]
struct Database {
    wb_presets: Vec<Maker>,
}

#[derive(Deserialize)]
struct Maker {
    maker: String,
    models: Vec<Model>,
}

#[derive(Deserialize)]
struct Model {
    model: String,
    presets: Vec<Preset>,
}

#[derive(Deserialize)]
struct Preset {
    name: String,
    channels: [f32; 4],
}

fn normalized_camera_name(value: &str) -> String {
    value
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .flat_map(char::to_uppercase)
        .collect()
}

pub fn for_camera(camera_maker: &str, camera_model: &str) -> Vec<WhiteBalancePreset> {
    type Catalog = HashMap<(String, String), Vec<WhiteBalancePreset>>;
    static CATALOG: OnceLock<Option<Catalog>> = OnceLock::new();
    let Some(catalog) = CATALOG
        .get_or_init(|| {
            let database: Database = serde_json::from_str(include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../data/wb_presets.json"
            )))
            .ok()?;
            Some(
                database
                    .wb_presets
                    .into_iter()
                    .flat_map(|maker| {
                        let maker_key = normalized_camera_name(&maker.maker);
                        maker.models.into_iter().map(move |model| {
                            let key = (maker_key.clone(), normalized_camera_name(&model.model));
                            let presets = model
                                .presets
                                .into_iter()
                                .filter(|preset| {
                                    preset.channels[..3]
                                        .iter()
                                        .all(|value| value.is_finite() && *value > 0.0)
                                })
                                .map(|preset| WhiteBalancePreset {
                                    name: preset.name,
                                    coefficients: preset.channels,
                                })
                                .collect();
                            (key, presets)
                        })
                    })
                    .collect(),
            )
        })
        .as_ref()
    else {
        return Vec::new();
    };
    catalog
        .get(&(
            normalized_camera_name(camera_maker),
            normalized_camera_name(camera_model),
        ))
        .cloned()
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::for_camera;

    #[test]
    fn bundled_darktable_database_matches_camera_names_robustly() {
        let presets = for_camera("SONY", "ILCE-7CM2");
        assert!(presets.iter().any(|preset| preset.name == "Daylight"));
        assert!(presets.iter().any(|preset| preset.name == "8500K"));
    }
}
