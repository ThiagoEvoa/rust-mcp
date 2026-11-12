//! The typed MCP tool/resource schemas for task `[048][M4]`, AC1.
//!
//! This module defines the **contract types** the five tools
//! (`list_sources`, `search_docs`, `get_section`, `get_api_checklist`,
//! `get_command_guidance`) and the two catalog/section resources speak. Each
//! type derives [`serde::Serialize`], [`serde::Deserialize`], and
//! [`schemars::JsonSchema`], so the JSON Schema the server advertises is
//! generated *from the same type* that serialises / deserialises the value —
//! the schema and the (de)serialisation cannot drift apart by construction.
//!
//! AC1 is proved by [`assert_schema_agrees`]: for each type, the JSON a value
//! *serialises to* is checked against the JSON Schema the *same type* generates
//! (a hand-written conformance check over the subset of JSON Schema that
//! `schemars` emits for these types). Inputs are checked the other way: a
//! representative request JSON must both **conform** to the input schema and
//! **deserialise** into the typed request. A cross-check also confirms that the
//! internal retrieval outputs ([`ChecklistEntry`](crate::model::ChecklistEntry),
//! [`CommandEntry`](crate::model::CommandEntry)) serialise to JSON that
//! conforms to the matching view schema, so the advertised schema agrees with
//! the real retrieval output, not just a hand-built example.
//!
//! Identity fields (section / source / snapshot ids, cursors, hashes,
//! timestamps) are plain `String`s on the wire — the typed
//! [`crate::model::SectionId`] / [`crate::model::SnapshotId`] / etc. validate
//! them on the way in; the wire stays simple and stable.
//
// Transitional: these contract types are the API surface that the [049]+
// tool/resource handler dispatch consumes (it advertises the schemas over the
// wire and deserialises requests into the typed requests). Nothing in the
// binary or the wired modules reaches them yet, so `dead_code` is silenced for
// this module only, mirroring the precedent in `runtime` / `ingest/*`.
#![allow(dead_code)]

use rmcp::model::{Resource, Tool};
use schemars::JsonSchema;
use schemars::generate::SchemaSettings;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

// ===========================================================================
// Shared views
// ===========================================================================

/// A single upstream citation a section or entry points at.
///
/// `url` / `title` / `note` are optional so an entry with only an in-corpus
/// reference (no canonical URL) still serialises cleanly.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct CitationView {
    /// The section this citation resolves to.
    pub section_id: String,
    /// The owning source collection.
    pub source: String,
    /// The canonical upstream URL, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// The cited document's title, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// A short human note (e.g. which part of the section is relevant).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// The per-source status row in [`ListSourcesResponse`] / the catalog resource.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SourceStatusView {
    /// The source identity (e.g. `cargo`).
    pub id: String,
    /// The human title of the collection.
    pub title: String,
    /// The last successful upstream check, ISO-8601; `null` when unknown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_checked: Option<String>,
    /// When this content was downloaded, ISO-8601.
    pub fetched_at: String,
    /// The known upstream modification time, ISO-8601; `null` when unknown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upstream_modified: Option<String>,
    /// The captured-content identity.
    pub content_hash: String,
    /// The freshness verdict (`stale` / `as_of` / `unknown` …).
    pub freshness: String,
}

/// A retrievable section, as returned by `get_section`, the section resource,
/// and the section bodies inside `get_command_guidance`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SectionView {
    /// The section identity.
    pub id: String,
    /// The owning source collection.
    pub source: String,
    /// The document's title.
    pub doc_title: String,
    /// The normalized chapter path the section was split from.
    pub chapter_path: String,
    /// The heading breadcrumb, most-to-least specific.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub heading_path: Vec<String>,
    /// The canonical upstream URL.
    pub canonical_url: String,
    /// The normalized Markdown body, verbatim.
    pub body: String,
    /// The active snapshot's identity.
    pub snapshot_id: String,
    /// The owning source's freshness verdict.
    pub freshness: String,
    /// The content hash identifying this section's body.
    pub content_hash: String,
}

/// One ranked search hit.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SearchHitView {
    /// The section this hit resolves to.
    pub section_id: String,
    /// The owning source collection.
    pub source: String,
    /// The document's title.
    pub title: String,
    /// A short excerpt of the matching text.
    pub excerpt: String,
    /// The deterministic BM25 score.
    pub score: f64,
    /// Citations backing the hit.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub citations: Vec<CitationView>,
}

/// One API-guideline entry, mirroring
/// [`ChecklistEntry`](crate::model::ChecklistEntry).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ChecklistEntryView {
    /// The upstream guideline code (e.g. `C-CONV`).
    pub code: String,
    /// The entry's title.
    pub title: String,
    /// The upstream checklist category.
    pub category: String,
    /// The entry's location within the checklist.
    pub location: String,
    /// The section IDs that explain this entry.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub explanation_sections: Vec<String>,
}

/// A maintainer-authored effect note, kept distinct from upstream content,
/// mirroring [`EffectNote`](crate::model::EffectNote).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct EffectNoteView {
    /// The effect family (`source_change`, `lockfile_change`, …).
    pub kind: String,
    /// The human-readable note describing a *possible* effect.
    pub detail: String,
}

/// A command-catalog entry, mirroring
/// [`CommandEntry`](crate::model::CommandEntry).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct CommandEntryView {
    /// The canonical command key (e.g. `cargo test`).
    pub key: String,
    /// The command's documented purpose.
    pub purpose: String,
    /// The upstream sections this guidance cites.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub upstream_sections: Vec<String>,
    /// Documented syntax / option excerpts.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub syntax: Vec<String>,
    /// Documented examples (never executed).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub examples: Vec<String>,
    /// Documented prerequisites.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub prerequisites: Vec<String>,
    /// Documented version / stability qualifications.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub compatibility: Vec<String>,
    /// Maintainer-authored effect notes, kept separate from upstream content.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub effect_notes: Vec<EffectNoteView>,
}

// ===========================================================================
// Tool input types
// ===========================================================================

/// `list_sources` takes no parameters.
///
/// An empty request; its input schema is the empty object schema, so a client
/// may call it with `{}`. An *empty braced* struct (not a unit struct) is
/// required: a unit struct serialises to JSON `null`, which would advertise
/// `"type": "null"` instead of the empty `"object"` a no-arg tool needs.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize, JsonSchema)]
pub struct ListSourcesRequest {}

/// `search_docs` request: a query over the corpus, optionally scoped to a
/// source and paginated.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SearchDocsRequest {
    /// The search term.
    pub query: String,
    /// Restrict to one source collection; `None` searches all.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// The maximum hits to return; `None` uses the server default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
    /// A continuation cursor from a previous, *identical* call.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
}

/// `get_section` request: read one section by id, optionally resuming a page.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct GetSectionRequest {
    /// The section identity to read.
    pub section_id: String,
    /// A continuation cursor from a previous, *identical* call.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
}

/// `get_api_checklist` request: list guideline entries, optionally by category
/// and paginated.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct GetApiChecklistRequest {
    /// Restrict to one checklist category; `None` lists all.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    /// The maximum entries to return; `None` uses the server default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
    /// A continuation cursor from a previous, *identical* call.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
}

/// `get_command_guidance` request: read one command's guidance by key,
/// optionally paginated.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct GetCommandGuidanceRequest {
    /// The canonical command key (e.g. `cargo test`).
    pub command: String,
    /// The maximum section bodies to return; `None` uses the server default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
    /// A continuation cursor from a previous, *identical* call.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
}

// ===========================================================================
// Tool output types
// ===========================================================================

/// `list_sources` output: the snapshot identity plus one status row per source
/// and the discoverable command-catalog keys.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ListSourcesResponse {
    /// The active snapshot's identity.
    pub snapshot_id: String,
    /// The corpus content hash.
    pub corpus_hash: String,
    /// One status row per source collection.
    pub sources: Vec<SourceStatusView>,
    /// The command-catalog keys, for `get_command_guidance` discovery.
    pub commands: Vec<String>,
    /// The snapshot's schema version.
    pub schema_version: String,
}

/// `search_docs` output: the ranked hits and pagination state.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SearchDocsResponse {
    /// The active snapshot's identity.
    pub snapshot_id: String,
    /// The ranked hits, best first.
    pub hits: Vec<SearchHitView>,
    /// The limit that was applied.
    pub limit: u32,
    /// Whether the result was truncated by the response cap.
    pub truncated: bool,
    /// The next page cursor, when there is more.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    /// Suggested follow-up calls when the result is empty or truncated.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub next_steps: Vec<String>,
}

/// `get_section` output: one section and its pagination state.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct GetSectionResponse {
    /// The requested section.
    pub section: SectionView,
    /// The next page cursor, when the body was truncated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    /// Whether the body was truncated by the response cap.
    pub truncated: bool,
    /// The snapshot's schema version.
    pub schema_version: String,
}

/// `get_api_checklist` output: the matching entries and pagination state.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct GetApiChecklistResponse {
    /// The category filter that was applied, when one was given.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    /// The matching guideline entries.
    pub entries: Vec<ChecklistEntryView>,
    /// The active snapshot's identity.
    pub snapshot_id: String,
    /// The limit that was applied.
    pub limit: u32,
    /// Whether the result was truncated by the response cap.
    pub truncated: bool,
    /// The next page cursor, when there is more.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

/// `get_command_guidance` output: the command entry plus its section bodies and
/// pagination state.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct GetCommandGuidanceResponse {
    /// The command's catalog entry (metadata + effect notes).
    pub entry: CommandEntryView,
    /// The resolved section bodies the guidance cites, as a page.
    pub sections: Vec<SectionView>,
    /// The active snapshot's identity.
    pub snapshot_id: String,
    /// The limit that was applied.
    pub limit: u32,
    /// Whether the result was truncated by the response cap.
    pub truncated: bool,
    /// The next page cursor, when there is more.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

// ===========================================================================
// Resource types
// ===========================================================================

/// The `rust-docs://catalog` resource: the bounded, paginated catalog of
/// sources and command entries for the active snapshot.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct CatalogResourceView {
    /// The active snapshot's identity.
    pub snapshot_id: String,
    /// The corpus content hash.
    pub corpus_hash: String,
    /// The per-source status rows on this page.
    pub sources: Vec<SourceStatusView>,
    /// The command-catalog entries on this page.
    pub commands: Vec<CommandEntryView>,
    /// The next page cursor, when the catalog is truncated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    /// Whether the catalog was truncated by the response cap.
    pub truncated: bool,
}

/// The `rust-docs://section/<id>` resource: one section's content and
/// provenance.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SectionResourceView {
    /// The section, with its body, heading breadcrumb, and provenance.
    pub section: SectionView,
}

// ===========================================================================
// Schema generation + tool/resource builders
// ===========================================================================

/// The input (deserialisation-contract) JSON Schema for a tool's request type,
/// generated the same way `rmcp` builds a tool's `input_schema`
/// (`schemars` `draft2020_12`, deserialisation contract).
pub fn input_schema<T: JsonSchema + 'static>() -> Value {
    let schema = SchemaSettings::draft2020_12()
        .into_generator()
        .into_root_schema_for::<T>();
    serde_json::to_value(&schema).expect("root schema is serialisable")
}

/// The output (serialisation-contract) JSON Schema for a tool's response type,
/// generated the same way `rmcp` builds a tool's `output_schema`
/// (`schemars` `draft2020_12`, serialisation contract).
pub fn output_schema<T: JsonSchema + 'static>() -> Value {
    let schema = SchemaSettings::draft2020_12()
        .for_serialize()
        .into_generator()
        .into_root_schema_for::<T>();
    serde_json::to_value(&schema).expect("root schema is serialisable")
}

/// Build the `rmcp` [`Tool`] for a named tool with its typed input/output
/// schemas. This is the seam task [050]–[053] register against the handler.
pub fn tool<I, O>(name: &'static str, description: &'static str) -> Tool
where
    I: JsonSchema + 'static,
    O: JsonSchema + 'static,
{
    let input_schema = input_schema::<I>();
    let mut tool = Tool::new(
        name,
        description,
        std::sync::Arc::new(input_schema.as_object().cloned().unwrap_or_default()),
    );
    // `with_output_schema` would regenerate; set the output schema directly so it
    // is the serialisation-contract schema this module already computed.
    tool = tool.with_raw_output_schema(std::sync::Arc::new(
        output_schema::<O>()
            .as_object()
            .cloned()
            .unwrap_or_default(),
    ));
    tool
}

/// Build the `rmcp` [`Resource`] for a catalog or section resource with its
/// typed payload schema and `application/json` MIME type.
pub fn resource(uri: &str, name: &str, title: &str, description: &str) -> Resource {
    Resource::new(uri, name)
        .with_title(title)
        .with_description(description)
        .with_mime_type("application/json")
}

// ===========================================================================
// The five tools and two resources, named, for registration by [050]–[055].
// ===========================================================================

/// The `list_sources` tool.
pub fn list_sources_tool() -> Tool {
    tool::<ListSourcesRequest, ListSourcesResponse>(
        "list_sources",
        "List the source collections this snapshot serves, with per-source \
         freshness and the available command-catalog keys. Takes no arguments.",
    )
}

/// The `search_docs` tool.
pub fn search_docs_tool() -> Tool {
    tool::<SearchDocsRequest, SearchDocsResponse>(
        "search_docs",
        "Search the Rust documentation corpus by query, optionally scoped to a \
         source and paginated. Returns ranked hits with excerpts and citations.",
    )
}

/// The `get_section` tool.
pub fn get_section_tool() -> Tool {
    tool::<GetSectionRequest, GetSectionResponse>(
        "get_section",
        "Read one documentation section by its id, with its heading breadcrumb, \
         body, and provenance. Resumable via a continuation cursor.",
    )
}

/// The `get_api_checklist` tool.
pub fn get_api_checklist_tool() -> Tool {
    tool::<GetApiChecklistRequest, GetApiChecklistResponse>(
        "get_api_checklist",
        "List Rust API-guideline checklist entries, optionally filtered by \
         category and paginated.",
    )
}

/// The `get_command_guidance` tool.
pub fn get_command_guidance_tool() -> Tool {
    tool::<GetCommandGuidanceRequest, GetCommandGuidanceResponse>(
        "get_command_guidance",
        "Read the guidance for one command (e.g. `cargo test`): its purpose, \
         syntax, examples, prerequisites, compatibility, effect notes, and the \
         source-backed sections it cites.",
    )
}

/// The `rust-docs://catalog` resource.
pub fn catalog_resource() -> Resource {
    resource(
        "rust-docs://catalog",
        "catalog",
        "Rust documentation catalog",
        "The bounded catalog of source collections and command entries for the \
         active snapshot. Paginated via a continuation cursor.",
    )
}

/// The `rust-docs://section/<id>` resource template.
pub fn section_resource() -> Resource {
    resource(
        "rust-docs://section/{id}",
        "section",
        "Documentation section",
        "One documentation section by id, with its body, heading breadcrumb, \
         and provenance.",
    )
}

/// All five tools, in a stable order, for [`rmcp::ServerHandler::list_tools`].
pub fn all_tools() -> Vec<Tool> {
    vec![
        list_sources_tool(),
        search_docs_tool(),
        get_section_tool(),
        get_api_checklist_tool(),
        get_command_guidance_tool(),
    ]
}

/// The two resources, for [`rmcp::ServerHandler::list_resources`].
pub fn all_resources() -> Vec<Resource> {
    vec![catalog_resource(), section_resource()]
}

// ===========================================================================
// AC1: schema-validation agreement
// ===========================================================================

/// A conformance failure: the value does not satisfy the schema at `path`.
#[derive(Debug, PartialEq, Eq)]
pub struct ConformanceError {
    /// The dotted path of the value that failed (e.g. `hits[0].score`).
    pub path: String,
    /// Why it failed.
    pub reason: String,
}

impl std::fmt::Display for ConformanceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.path, self.reason)
    }
}

impl std::error::Error for ConformanceError {}

/// Check that `value` conforms to `root_schema` over the subset of JSON Schema
/// that `schemars` emits for these types (`type`, `properties`, `required`,
/// `items`, `anyOf`/`oneOf`/`allOf`, `enum`, `$ref`→`definitions`/`$defs`).
///
/// Unknown keywords are ignored, matching how a real validator treats the
/// keywords it does not implement; the values checked here are generated by the
/// very types that produced the schema, so structural conformance is the
/// meaningful invariant.
pub fn value_conforms(root_schema: &Value, value: &Value) -> Result<(), ConformanceError> {
    let defs = root_schema
        .get("definitions")
        .or_else(|| root_schema.get("$defs"))
        .and_then(|d| d.as_object())
        .cloned()
        .unwrap_or_default();
    check(root_schema, value, &defs, "$", 0)
}

fn check(
    schema: &Value,
    value: &Value,
    defs: &Map<String, Value>,
    path: &str,
    depth: usize,
) -> Result<(), ConformanceError> {
    // A bare `true` schema accepts anything; `false` accepts nothing.
    match schema {
        Value::Bool(true) => return Ok(()),
        Value::Bool(false) => {
            return Err(ConformanceError {
                path: path.to_string(),
                reason: "schema is `false` (rejects all values)".to_string(),
            });
        }
        _ => {}
    }

    // Resolve a `$ref` to a definition and continue there.
    if let Some(ref_str) = schema.get("$ref").and_then(|r| r.as_str()) {
        let name = ref_str
            .strip_prefix("#/definitions/")
            .or_else(|| ref_str.strip_prefix("#/$defs/"))
            .or_else(|| ref_str.strip_prefix("#/"))
            .unwrap_or(ref_str);
        let target = defs.get(name).ok_or(ConformanceError {
            path: path.to_string(),
            reason: format!("unknown $ref '{ref_str}'"),
        })?;
        if depth > 32 {
            return Err(ConformanceError {
                path: path.to_string(),
                reason: "recursion limit exceeded resolving $ref".to_string(),
            });
        }
        return check(target, value, defs, path, depth + 1);
    }

    // Combinators: the value must satisfy the relevant branch(es).
    if let Some(any) = schema.get("anyOf").and_then(|a| a.as_array()) {
        let ok = any
            .iter()
            .any(|branch| check(branch, value, defs, path, depth + 1).is_ok());
        if !ok {
            return Err(ConformanceError {
                path: path.to_string(),
                reason: "value matches none of the anyOf branches".to_string(),
            });
        }
    }
    if let Some(one) = schema.get("oneOf").and_then(|a| a.as_array()) {
        let n = one
            .iter()
            .filter(|branch| check(branch, value, defs, path, depth + 1).is_ok())
            .count();
        if n != 1 {
            return Err(ConformanceError {
                path: path.to_string(),
                reason: format!("value matches {n} oneOf branches (expected exactly 1)"),
            });
        }
    }
    if let Some(all) = schema.get("allOf").and_then(|a| a.as_array()) {
        for branch in all {
            check(branch, value, defs, path, depth + 1)?;
        }
    }

    // `enum`: the value must equal one of the listed values.
    if let Some(list) = schema.get("enum").and_then(|e| e.as_array())
        && !list.iter().any(|e| e == value)
    {
        return Err(ConformanceError {
            path: path.to_string(),
            reason: "value is not one of the enum values".to_string(),
        });
    }

    // `type`: string, or an array of allowed types.
    if let Some(types) = schema.get("type") {
        let mut ok = false;
        if let Some(t) = types.as_str() {
            ok = type_matches(t, value);
        } else if let Some(ts) = types.as_array() {
            ok = ts
                .iter()
                .any(|t| t.as_str().is_some_and(|s| type_matches(s, value)));
        }
        if !ok {
            return Err(ConformanceError {
                path: path.to_string(),
                reason: format!("value is not of the required type {types}"),
            });
        }
    }

    // Object shape: required properties present, present properties conform.
    if let (Value::Object(obj), Some(props)) =
        (value, schema.get("properties").and_then(|p| p.as_object()))
    {
        if let Some(req) = schema.get("required").and_then(|r| r.as_array()) {
            for r in req {
                let name = r.as_str().unwrap_or_default();
                if !obj.contains_key(name) {
                    return Err(ConformanceError {
                        path: path.to_string(),
                        reason: format!("missing required property '{name}'"),
                    });
                }
            }
        }
        for (name, sub) in props {
            if let Some(v) = obj.get(name) {
                check(sub, v, defs, &format!("{path}.{name}"), depth + 1)?;
            }
        }
        if let Some(additional) = schema.get("additionalProperties")
            && additional == &Value::Bool(false)
        {
            for name in obj.keys() {
                if !props.contains_key(name) {
                    return Err(ConformanceError {
                        path: path.to_string(),
                        reason: format!(
                            "unexpected property '{name}' (additionalProperties=false)"
                        ),
                    });
                }
            }
        }
    }

    // Array shape: each item conforms.
    if let (Value::Array(items), Some(item_schema)) = (value, schema.get("items"))
        && item_schema.is_object()
    {
        for (i, item) in items.iter().enumerate() {
            check(item_schema, item, defs, &format!("{path}[{i}]"), depth + 1)?;
        }
    }

    Ok(())
}

fn type_matches(t: &str, value: &Value) -> bool {
    match t {
        "object" => value.is_object(),
        "array" => value.is_array(),
        "string" => value.is_string(),
        "boolean" => value.is_boolean(),
        "null" => value.is_null(),
        "integer" => value
            .as_number()
            .is_some_and(|n| n.as_i64().is_some() || n.as_u64().is_some()),
        "number" => value.is_number(),
        _ => false,
    }
}

/// Assert AC1 for one type: a value *serialised* by `T` conforms to the schema
/// *that same type generates*. `T` must round-trip through its own schema.
fn assert_output_agrees<T>()
where
    T: JsonSchema + Serialize + Default + 'static,
{
    let value = serde_json::to_value(T::default()).expect("default serialises");
    let schema = output_schema::<T>();
    value_conforms(&schema, &value).unwrap_or_else(|e| {
        panic!(
            "output schema for {} does not validate its own serialised default: {e}\n\
             value: {value}\nschema: {schema}",
            std::any::type_name::<T>()
        )
    });
}

/// Assert AC1 for a request type: a representative request JSON both conforms to
/// the input schema and deserialises into `T`.
fn assert_input_roundtrips<T>()
where
    T: JsonSchema + DeserializeOwnedLike + 'static,
{
    let json = representative_input::<T>();
    let schema = input_schema::<T>();
    value_conforms(&schema, &json).unwrap_or_else(|e| {
        panic!(
            "input schema for {} does not validate its representative input: {e}\n\
             input: {json}\nschema: {schema}",
            std::any::type_name::<T>()
        )
    });
    // The schema-agreement property is structural; also confirm it deserialises.
    let _parsed = T::from_value(&json);
}

/// A minimal `Deserialize`-like bound so the test helper can name the type.
///
/// (Kept as a trait object-free bound; the concrete calls use real
/// `Deserialize` types, so `from_value` just delegates to `serde_json`.)
trait DeserializeOwnedLike: for<'de> Deserialize<'de> + Sized {
    fn from_value(v: &Value) -> Self {
        serde_json::from_value(v.clone()).expect("representative input deserialises")
    }
}

impl<T: for<'de> Deserialize<'de>> DeserializeOwnedLike for T {}

fn representative_input<T>() -> Value {
    // Each request type has a small canonical example.
    let name = std::any::type_name::<T>();
    match name {
        n if n.ends_with("ListSourcesRequest") => json!({}),
        n if n.ends_with("SearchDocsRequest") => json!({
            "query": "how to run tests",
            "source": "cargo",
            "limit": 5,
            "cursor": "eyJvIjoic2VhcmNoIn0",
        }),
        n if n.ends_with("GetSectionRequest") => json!({
            "section_id": "cargo/01-build/1",
            "cursor": "eyJvIjoiZ2V0X3NlY3Rpb24ifQ",
        }),
        n if n.ends_with("GetApiChecklistRequest") => json!({
            "category": "Conventions",
            "limit": 10,
            "cursor": "eyJvIjoiZ2V0X2FwaV9jaGVja2xpc3QifQ",
        }),
        n if n.ends_with("GetCommandGuidanceRequest") => json!({
            "command": "cargo test",
            "limit": 3,
            "cursor": "eyJvIjoiZ2V0X2NvbW1hbmRfZ3VpZGFuY2UifQ",
        }),
        _ => panic!("no representative input for {name}"),
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use super::*;
    use crate::model::{ChecklistEntry, CommandEntry, EffectKind, EffectNote, SectionId};
    use serde_json::json;

    // ----- AC1: every output type's schema validates its own serialisation. -----

    #[test]
    fn output_schemas_validate_own_serialisation() {
        assert_output_agrees::<ListSourcesResponse>();
        assert_output_agrees::<SearchDocsResponse>();
        assert_output_agrees::<GetSectionResponse>();
        assert_output_agrees::<GetApiChecklistResponse>();
        assert_output_agrees::<GetCommandGuidanceResponse>();
        assert_output_agrees::<CatalogResourceView>();
        assert_output_agrees::<SectionResourceView>();
    }

    // ----- AC1: every input type's schema validates a representative input. -----

    #[test]
    fn input_schemas_validate_representative_inputs() {
        assert_input_roundtrips::<ListSourcesRequest>();
        assert_input_roundtrips::<SearchDocsRequest>();
        assert_input_roundtrips::<GetSectionRequest>();
        assert_input_roundtrips::<GetApiChecklistRequest>();
        assert_input_roundtrips::<GetCommandGuidanceRequest>();
    }

    #[test]
    fn input_schema_for_list_sources_is_an_object() {
        // `list_sources` takes no arguments: its input schema is the empty
        // object schema, so `{}` conforms and deserialises.
        let schema = input_schema::<ListSourcesRequest>();
        assert!(schema.get("type").and_then(|t| t.as_str()) == Some("object"));
        value_conforms(&schema, &json!({})).expect("empty object conforms");
        let parsed = ListSourcesRequest::from_value(&json!({}));
        let _ = parsed;
    }

    #[test]
    fn representative_search_request_deserialises() {
        let req = SearchDocsRequest::from_value(&json!({
            "query": "rust async",
            "limit": 4,
        }));
        assert_eq!(req.query, "rust async");
        assert_eq!(req.limit, Some(4));
        assert!(req.source.is_none());
        assert!(req.cursor.is_none());
    }

    // ----- AC1: the registered tools/resources carry coherent schemas. -----

    #[test]
    fn all_five_tools_have_input_and_output_schemas() {
        let tools = all_tools();
        assert_eq!(tools.len(), 5, "the five planned tools");
        for t in &tools {
            assert!(!t.name.is_empty());
            assert!(t.description.is_some());
            let input = t.input_schema.as_ref();
            assert!(
                input.get("type").and_then(|x| x.as_str()) == Some("object"),
                "tool {} input schema is an object",
                t.name
            );
            let out = t
                .output_schema
                .as_ref()
                .expect("each tool advertises an output schema");
            assert!(
                out.get("type").and_then(|x| x.as_str()) == Some("object"),
                "tool {} output schema is an object",
                t.name
            );
        }
    }

    #[test]
    fn tool_names_are_stable() {
        let tools = all_tools();
        let names: Vec<&str> = tools.iter().map(|t| t.name.as_ref()).collect();
        assert_eq!(
            names,
            vec![
                "list_sources",
                "search_docs",
                "get_section",
                "get_api_checklist",
                "get_command_guidance"
            ]
        );
    }

    #[test]
    fn two_resources_are_json() {
        let resources = all_resources();
        assert_eq!(resources.len(), 2);
        for r in &resources {
            assert_eq!(r.mime_type.as_deref(), Some("application/json"));
        }
    }

    // ----- AC1: advertised schema agrees with the *internal* retrieval output.

    #[test]
    fn checklist_entry_view_agrees_with_internal_serialisation() {
        let internal = ChecklistEntry {
            code: "C-CONV".to_string(),
            title: "Title".to_string(),
            category: "Conventions".to_string(),
            location: "1.2".to_string(),
            explanation_sections: vec![
                SectionId::from_str("api-guidelines/rust-2024").expect("sid"),
            ],
        };
        let json = serde_json::to_value(&internal).expect("ChecklistEntry serialises");
        let schema = output_schema::<ChecklistEntryView>();
        value_conforms(&schema, &json).unwrap_or_else(|e| {
            panic!("internal ChecklistEntry does not conform to ChecklistEntryView: {e}\n{json}")
        });
    }

    #[test]
    fn command_entry_view_agrees_with_internal_serialisation() {
        let internal = CommandEntry {
            key: "cargo test".parse().expect("key"),
            purpose: "Run tests".to_string(),
            upstream_sections: vec![SectionId::from_str("cargo/01-build").expect("sid")],
            syntax: vec!["cargo test".to_string()],
            examples: vec!["cargo test --release".to_string()],
            prerequisites: vec!["a test target".to_string()],
            compatibility: vec!["stable".to_string()],
            effect_notes: vec![EffectNote {
                kind: EffectKind::ProjectCodeExecution,
                detail: "compiles and runs the project's code".to_string(),
            }],
        };
        let json = serde_json::to_value(&internal).expect("CommandEntry serialises");
        let schema = output_schema::<CommandEntryView>();
        value_conforms(&schema, &json).unwrap_or_else(|e| {
            panic!("internal CommandEntry does not conform to CommandEntryView: {e}\n{json}")
        });
    }

    // ----- The conformance checker itself. -----

    #[test]
    fn checker_rejects_wrong_types() {
        let schema = output_schema::<GetSectionRequest>();
        // A string is not an object.
        assert!(value_conforms(&schema, &json!("not an object")).is_err());
        // An object missing `section_id` fails (it is required).
        assert!(value_conforms(&schema, &json!({})).is_err());
        // A well-formed object passes.
        assert!(value_conforms(&schema, &json!({"section_id": "x/1"})).is_ok());
    }

    #[test]
    fn checker_rejects_wrong_property_types() {
        let schema = output_schema::<SearchDocsResponse>();
        // `limit` must be an integer; a string fails.
        let bad = json!({
            "snapshot_id": "s",
            "hits": [],
            "limit": "not-a-number",
            "truncated": false,
            "next_cursor": null,
            "next_steps": []
        });
        assert!(value_conforms(&schema, &bad).is_err());
    }

    #[test]
    fn checker_handles_optional_and_nested_arrays() {
        let schema = output_schema::<SearchDocsResponse>();
        // A hit with citations, and `next_cursor` present, must conform.
        let good = json!({
            "snapshot_id": "s",
            "hits": [
                {
                    "section_id": "cargo/01-build/1",
                    "source": "cargo",
                    "title": "t",
                    "excerpt": "e",
                    "score": 1.5,
                    "citations": [
                        { "section_id": "cargo/01-build/1", "source": "cargo", "url": "https://x" }
                    ]
                }
            ],
            "limit": 5,
            "truncated": true,
            "next_cursor": "abc",
            "next_steps": ["try another query"]
        });
        assert!(value_conforms(&schema, &good).is_ok());
    }
}
