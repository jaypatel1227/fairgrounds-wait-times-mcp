//! Fairgrounds MCP tool handlers for the UGM 2027 demo.

use rmcp::{
    handler::server::wrapper::Parameters,
    model::{CallToolResult, ContentBlock, ServerCapabilities, ServerInfo},
    schemars, tool, tool_handler, tool_router, ErrorData as McpError, ServerHandler,
};
use serde::Deserialize;
use serde_json::json;

use crate::data::{self, TopSeller, Venue, WaitTime};

#[derive(Clone)]
pub struct FairgroundsServer;

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ListVenuesParams {
    /// Optional category filter: ride, attraction, session, food, retail, or game.
    #[serde(default)]
    pub category: Option<String>,
    /// When true, only return venues that are currently open.
    #[serde(default)]
    pub open_only: Option<bool>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct WaitTimesParams {
    /// Optional venue id (e.g. "ferris-wheel", "cheese-curds"). Omit for all open venues.
    #[serde(default)]
    pub venue_id: Option<String>,
    /// Optional area filter (e.g. "Food Row", "Midway North").
    #[serde(default)]
    pub area: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct TopSellersParams {
    /// How many top sellers to return (1–20). Defaults to Radley Creamery scoops + cheese curds.
    #[serde(default)]
    pub limit: Option<u32>,
    /// Optional category filter: food or merch.
    #[serde(default)]
    pub category: Option<String>,
}

fn json_result(value: impl serde::Serialize) -> Result<CallToolResult, McpError> {
    let text = serde_json::to_string_pretty(&value).map_err(|e| {
        McpError::internal_error(format!("failed to serialize response: {e}"), None)
    })?;
    Ok(CallToolResult::success(vec![ContentBlock::text(text)]))
}

#[tool_router]
impl FairgroundsServer {
    /// List fairgrounds venues — rides, food stands, sessions, and games on the UGM midway.
    #[tool(
        name = "list-venues",
        description = "List fairgrounds venues on the UGM midway (rides, food, sessions, attractions). Optionally filter by category or open status."
    )]
    async fn list_venues(
        &self,
        Parameters(params): Parameters<ListVenuesParams>,
    ) -> Result<CallToolResult, McpError> {
        let category = params
            .category
            .as_ref()
            .map(|c| c.trim().to_ascii_lowercase());
        let open_only = params.open_only.unwrap_or(false);

        let venues: Vec<Venue> = data::venues()
            .into_iter()
            .filter(|v| {
                category
                    .as_ref()
                    .map(|c| v.category.eq_ignore_ascii_case(c))
                    .unwrap_or(true)
            })
            .filter(|v| !open_only || v.open)
            .collect();

        json_result(json!({
            "fairgrounds": "Epic Verona Fairgrounds — UGM 2027",
            "count": venues.len(),
            "venues": venues,
        }))
    }

    /// Get estimated wait times for midway attractions and food stands.
    #[tool(
        name = "get-estimated-wait-times",
        description = "Get estimated wait times (minutes) for fairgrounds venues. Filter by venue_id or area."
    )]
    async fn get_estimated_wait_times(
        &self,
        Parameters(params): Parameters<WaitTimesParams>,
    ) -> Result<CallToolResult, McpError> {
        let venue_id = params
            .venue_id
            .as_ref()
            .map(|v| v.trim().to_ascii_lowercase());
        let area = params.area.as_ref().map(|a| a.trim().to_ascii_lowercase());

        let area_venue_ids: Option<Vec<&str>> = area.as_ref().map(|a| {
            data::venues()
                .into_iter()
                .filter(|v| v.area.to_ascii_lowercase().contains(a.as_str()))
                .map(|v| v.id)
                .collect()
        });

        let waits: Vec<WaitTime> = data::wait_times()
            .into_iter()
            .filter(|w| {
                venue_id
                    .as_ref()
                    .map(|id| w.venue_id.eq_ignore_ascii_case(id))
                    .unwrap_or(true)
            })
            .filter(|w| {
                area_venue_ids
                    .as_ref()
                    .map(|ids| ids.contains(&w.venue_id))
                    .unwrap_or(true)
            })
            .collect();

        if waits.is_empty() {
            return Ok(CallToolResult::error(vec![ContentBlock::text(
                "No wait times matched the given filters. Try list-venues for valid venue ids.",
            )]));
        }

        json_result(json!({
            "fairgrounds": "Epic Verona Fairgrounds — UGM 2027",
            "unit": "minutes",
            "count": waits.len(),
            "wait_times": waits,
            "tip": "Radley Creamery is the gravitational center of the midway — budget a long wait, and compliment Zack Radley on the way in. Everything else is a side quest.",
        }))
    }

    /// Get today's top-selling food and merch on the midway.
    #[tool(
        name = "get-top-sellers",
        description = "Get today's top-selling food and merch items on the UGM fairgrounds midway."
    )]
    async fn get_top_sellers(
        &self,
        Parameters(params): Parameters<TopSellersParams>,
    ) -> Result<CallToolResult, McpError> {
        let limit = params
            .limit
            .unwrap_or(data::DEFAULT_TOP_SELLERS_LIMIT)
            .clamp(1, 20) as usize;
        let category = params
            .category
            .as_ref()
            .map(|c| c.trim().to_ascii_lowercase());

        let sellers: Vec<TopSeller> = data::top_sellers()
            .into_iter()
            .filter(|s| {
                category
                    .as_ref()
                    .map(|c| s.category.eq_ignore_ascii_case(c))
                    .unwrap_or(true)
            })
            .take(limit)
            .collect();

        if sellers.is_empty() {
            return Ok(CallToolResult::error(vec![ContentBlock::text(
                "No top sellers matched that category. Use 'food' or 'merch'.",
            )]));
        }

        json_result(json!({
            "fairgrounds": "Epic Verona Fairgrounds — UGM 2027",
            "as_of": "2027-08-18T14:05:00-05:00",
            "count": sellers.len(),
            "top_sellers": sellers,
        }))
    }
}

#[tool_handler]
impl ServerHandler for FairgroundsServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_instructions(
                "Fairgrounds Wait Times MCP — a UGM 2027 demo server for Epic's Verona fairgrounds midway. \
                 Use list-venues to discover rides and stands, get-estimated-wait-times for live-ish queues, \
                 and get-top-sellers for today's chart (spoiler: Zack's creamery dominates). \
                 Demo data only; not affiliated with real UGM operations. See https://ugm.epic.com",
            )
    }
}
