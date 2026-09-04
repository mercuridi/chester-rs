use crate::discord::context::Error;

pub const MOODS: &[&str] = &[
    "serene",
    "warm",
    "playful",
    "whimsical",
    "hopeful",
    "wistful",
    "somber",
    "mysterious",
    "eerie",
    "ominous",
    "menacing",
    "tense",
    "anxious",
    "majestic",
    "chaotic",
    "triumphant",
];
pub const INTENSITIES: &[&str] = &["subtle", "measured", "driving", "fierce"];
pub const FUNCTIONS: &[&str] = &[
    "exploratory",
    "investigative",
    "traveling",
    "social",
    "romantic",
    "combative",
    "climactic",
    "stealthy",
    "ceremonial",
    "celebratory",
    "contemplative",
    "conversational",
];
pub const TEXTURES: &[&str] = &[
    "ambient",
    "acoustic",
    "orchestral",
    "electronic",
    "synthetic",
    "folk",
    "piano",
    "percussive",
    "choral",
    "vocal",
    "minimalist",
    "ethereal",
    "dissonant",
];
pub const ENVIRONMENTS: &[&str] = &[
    "desert",
    "forest",
    "tundra",
    "mountainous",
    "coastal",
    "oceanic",
    "swampy",
    "urban",
    "rural",
    "underground",
    "ruined",
    "sacred",
    "otherworldly",
    "celestial",
    "infernal",
];

pub fn require_value(values: &[&str], value: &str, kind: &str) -> Result<(), Error> {
    if values.contains(&value) {
        Ok(())
    } else {
        Err(format!("Unknown {kind} `{value}`").into())
    }
}
