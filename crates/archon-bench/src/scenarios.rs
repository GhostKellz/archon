#[derive(Debug, Clone)]
pub struct LoadScenario {
    pub name: &'static str,
    pub description: &'static str,
    pub default_url: &'static str,
    pub default_iterations: u32,
    pub default_headless: bool,
    pub default_concurrency: u32,
    pub thresholds: LoadThresholds,
}

#[derive(Debug, Clone, Copy)]
pub struct LoadThresholds {
    pub max_first_contentful_paint_ms: Option<f64>,
    pub max_largest_contentful_paint_ms: Option<f64>,
    pub max_cumulative_layout_shift: Option<f64>,
    pub max_total_blocking_time_ms: Option<f64>,
    pub max_first_input_delay_ms: Option<f64>,
}

const LOAD_SCENARIOS: &[LoadScenario] = &[
    LoadScenario {
        name: "top-sites",
        description: "Static-first landing page mix representative of the Alexa top 50.",
        default_url: "https://www.wikipedia.org/",
        default_iterations: 3,
        default_headless: true,
        default_concurrency: 1,
        thresholds: LoadThresholds {
            max_first_contentful_paint_ms: Some(1200.0),
            max_largest_contentful_paint_ms: Some(2500.0),
            max_cumulative_layout_shift: Some(0.1),
            max_total_blocking_time_ms: Some(200.0),
            max_first_input_delay_ms: Some(100.0),
        },
    },
    LoadScenario {
        name: "news-heavy",
        description: "High-density news homepage emphasising LCP and CLS stress.",
        default_url: "https://www.theguardian.com/international",
        default_iterations: 4,
        default_headless: true,
        default_concurrency: 1,
        thresholds: LoadThresholds {
            max_first_contentful_paint_ms: Some(1800.0),
            max_largest_contentful_paint_ms: Some(3200.0),
            max_cumulative_layout_shift: Some(0.15),
            max_total_blocking_time_ms: Some(350.0),
            max_first_input_delay_ms: Some(125.0),
        },
    },
    LoadScenario {
        name: "social-feed",
        description: "Infinite-scroll style social feed with dynamic media content.",
        default_url: "https://www.reddit.com/",
        default_iterations: 5,
        default_headless: true,
        default_concurrency: 1,
        thresholds: LoadThresholds {
            max_first_contentful_paint_ms: Some(2200.0),
            max_largest_contentful_paint_ms: Some(3800.0),
            max_cumulative_layout_shift: Some(0.18),
            max_total_blocking_time_ms: Some(400.0),
            max_first_input_delay_ms: Some(150.0),
        },
    },
];

pub fn default_load_scenario() -> &'static LoadScenario {
    &LOAD_SCENARIOS[0]
}

pub fn load_scenarios() -> &'static [LoadScenario] {
    LOAD_SCENARIOS
}

pub fn find_load_scenario(name: &str) -> Option<&'static LoadScenario> {
    let needle = name.trim();
    if needle.is_empty() {
        return None;
    }
    LOAD_SCENARIOS
        .iter()
        .find(|scenario| scenario.name.eq_ignore_ascii_case(needle))
}
