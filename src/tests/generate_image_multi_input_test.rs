//! #2045: `generate_image` takes one input image or a list, on both the Gemini
//! and OpenAI-compatible backends. Gemini gets one `inlineData` part per image,
//! then the text; OpenAI-compatible inputs switch the call to `/images/edits`.

use crate::brain::tools::generate_image::{
    GenerateImageTool, gemini_request_parts, openai_images_path, parse_image_inputs,
};
use crate::brain::tools::r#trait::Tool;
use serde_json::{Value, json};

#[test]
fn a_single_string_is_one_input_as_before() {
    assert_eq!(
        parse_image_inputs(&json!("/tmp/a.png")).unwrap(),
        vec!["/tmp/a.png".to_string()]
    );
}

#[test]
fn an_array_is_every_input_in_order() {
    assert_eq!(
        parse_image_inputs(&json!(["/tmp/a.png", "https://example.test/b.jpg"])).unwrap(),
        vec![
            "/tmp/a.png".to_string(),
            "https://example.test/b.jpg".to_string()
        ]
    );
}

#[test]
fn missing_and_empty_inputs_mean_text_only() {
    assert!(parse_image_inputs(&Value::Null).unwrap().is_empty());
    assert!(parse_image_inputs(&json!("")).unwrap().is_empty());
    assert!(parse_image_inputs(&json!(["", ""])).unwrap().is_empty());
}

#[test]
fn a_non_string_entry_is_refused_not_silently_dropped() {
    assert!(parse_image_inputs(&json!(["/tmp/a.png", 7])).is_err());
    assert!(parse_image_inputs(&json!(42)).is_err());
}

#[test]
fn the_list_is_capped_per_call() {
    let nine: Vec<String> = (0..9).map(|i| format!("/tmp/{i}.png")).collect();
    assert!(parse_image_inputs(&json!(nine)).is_err());
    let eight: Vec<String> = (0..8).map(|i| format!("/tmp/{i}.png")).collect();
    assert_eq!(parse_image_inputs(&json!(eight)).unwrap().len(), 8);
}

#[test]
fn gemini_emits_one_part_per_image_then_the_text() {
    let images = vec![
        json!({"inlineData": {"data": "A"}}),
        json!({"inlineData": {"data": "B"}}),
    ];
    let parts = gemini_request_parts(images, "combine them");
    assert_eq!(parts.len(), 3);
    assert_eq!(parts[0]["inlineData"]["data"], "A");
    assert_eq!(parts[1]["inlineData"]["data"], "B");
    assert_eq!(parts[2]["text"], "combine them");
}

#[test]
fn a_single_image_still_produces_exactly_one_image_part() {
    let parts = gemini_request_parts(vec![json!({"inlineData": {"data": "A"}})], "edit");
    let images = parts
        .iter()
        .filter(|p| p.get("inlineData").is_some())
        .count();
    assert_eq!(images, 1);
    assert_eq!(parts.len(), 2);
}

#[test]
fn openai_routes_text_to_generations_and_inputs_to_edits() {
    assert_eq!(openai_images_path(false), "/images/generations");
    assert_eq!(openai_images_path(true), "/images/edits");
}

#[test]
fn the_schema_accepts_one_string_or_a_list() {
    let tool = GenerateImageTool::new("k".to_string(), "m".to_string());
    let schema = tool.input_schema();
    let any_of = schema["properties"]["image"]["anyOf"]
        .as_array()
        .expect("image is anyOf string or array");
    assert!(any_of.iter().any(|v| v["type"] == "string"));
    let list = any_of
        .iter()
        .find(|v| v["type"] == "array")
        .expect("array form");
    assert_eq!(list["items"]["type"], "string");
    assert_eq!(list["maxItems"], 8);
}
