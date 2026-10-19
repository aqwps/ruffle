use super::{DeviceFontRenderer, load_fontdb_font};
use fontdb::{Database, Family, Style, Weight};
use ruffle_core::backend::ui::FontDefinition;
use ruffle_core::font::{FontAtlases, FontQuery};

// Unlike fontconfig, fontdb only matches font families; it does not supply
// fallback faces for characters missing from the requested font. Include the
// common Windows CJK fonts so Latin fonts such as Arial can display CJK text.
const FALLBACK_FAMILIES: &[&str] = &[
    "SimSun",
    "Microsoft YaHei",
    "Microsoft JhengHei",
    "MingLiU",
    "Yu Gothic",
    "Meiryo",
    "MS Gothic",
    "Malgun Gothic",
    "Segoe UI Symbol",
];

pub(super) fn sort_device_fonts(
    database: &Database,
    query: &FontQuery,
    register: &mut dyn FnMut(FontDefinition),
    renderer: DeviceFontRenderer,
    atlases: &FontAtlases,
) -> Vec<FontQuery> {
    let mut fonts = Vec::new();
    let mut seen = Vec::new();
    for name in std::iter::once(query.name.as_str()).chain(FALLBACK_FAMILIES.iter().copied()) {
        let id = database.query(&fontdb::Query {
            families: &[Family::Name(name)],
            weight: if query.is_bold {
                Weight::BOLD
            } else {
                Weight::NORMAL
            },
            style: if query.is_italic {
                Style::Italic
            } else {
                Style::Normal
            },
            ..Default::default()
        });
        let Some(face) = id.and_then(|id| database.face(id)) else {
            // Let core choose the default family when the requested font is
            // unavailable, rather than replacing it with the first CJK face.
            if fonts.is_empty() {
                return fonts;
            }
            continue;
        };
        if seen.contains(&face.id) {
            continue;
        }
        seen.push(face.id);

        match load_fontdb_font(name.to_owned(), face, renderer, atlases) {
            Ok(definition) => {
                tracing::info!(
                    "Loading device font \"{}\" for \"{}\" (family: {name})",
                    face.post_script_name,
                    query.name,
                );
                register(definition);
                // fontdb can substitute a different style, so use the actual
                // face's flags to find the font we just registered in core.
                fonts.push(FontQuery::new(
                    query.font_type,
                    name.to_owned(),
                    face.weight > Weight::NORMAL,
                    face.style != Style::Normal,
                ));
            }
            Err(error) => {
                tracing::error!("Error loading font from fontdb: {error}");
                if fonts.is_empty() {
                    return fonts;
                }
            }
        }
    }
    fonts
}

#[cfg(test)]
mod tests {
    use super::*;
    use ruffle_core::font::{FontFace, FontType};

    fn test_database() -> Database {
        let mut database = Database::new();
        database.load_font_data(
            include_bytes!("../../../../tests/tests/swfs/visual/fonts/device-font/tinos.ttf")
                .to_vec(),
        );
        let template = database.faces().next().unwrap().clone();
        for names in [&["Arial"][..], &["SimSun", "宋体"], &["Microsoft YaHei"]] {
            let mut face = template.clone();
            face.families = names
                .iter()
                .map(|name| (name.to_string(), fontdb::Language::English_UnitedStates))
                .collect();
            database.push_face_info(face);
        }
        database
    }

    fn sorted(database: &Database, name: &str, bold: bool, italic: bool) -> Vec<FontQuery> {
        sort_device_fonts(
            database,
            &FontQuery::new(FontType::Device, name.to_owned(), bold, italic),
            &mut |_| {},
            DeviceFontRenderer::Embedded,
            &FontAtlases::new(),
        )
    }

    #[test]
    fn preserves_primary_and_appends_available_fallbacks() {
        let fonts = sorted(&test_database(), "Arial", false, false);
        assert_eq!(
            fonts
                .iter()
                .map(|font| font.name.as_str())
                .collect::<Vec<_>>(),
            ["Arial", "SimSun", "Microsoft YaHei"]
        );
    }

    #[test]
    fn missing_primary_leaves_default_family_selection_to_core() {
        assert!(sorted(&test_database(), "Missing font", false, false).is_empty());
    }

    #[test]
    fn aliases_do_not_duplicate_faces_and_substituted_styles_are_registered() {
        let fonts = sorted(&test_database(), "宋体", true, true);
        assert_eq!(
            fonts
                .iter()
                .map(|font| font.name.as_str())
                .collect::<Vec<_>>(),
            ["宋体", "Microsoft YaHei"]
        );
        assert!(fonts.iter().all(|font| !font.is_bold && !font.is_italic));
    }

    #[test]
    #[ignore = "requires Windows system fonts, including SimSun or Microsoft YaHei"]
    fn system_fallbacks_supply_chinese_glyphs_missing_from_arial() {
        let mut database = Database::new();
        database.load_system_fonts();
        for (bold, italic) in [(false, false), (true, false), (false, true), (true, true)] {
            let mut faces = Vec::new();
            let fonts = sort_device_fonts(
                &database,
                &FontQuery::new(FontType::Device, "Arial".into(), bold, italic),
                &mut |definition| {
                    if let FontDefinition::FontFile { data, index, .. } = definition {
                        faces.push(FontFace::new(data, index).unwrap());
                    }
                },
                DeviceFontRenderer::Embedded,
                &FontAtlases::new(),
            );
            assert_eq!(fonts[0].name, "Arial");
            assert!(faces[0].get_glyph('中').is_none());
            for character in "中文袁守诚名字简介123".chars() {
                assert!(faces.iter().any(|face| face.get_glyph(character).is_some()));
            }
        }
    }
}
