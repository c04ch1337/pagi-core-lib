//! Phoenix AGI (PAGI) shared core library.
//!
//! This crate defines:
//! - [`Task`]: a minimal task envelope used by the planner to dispatch work to agents.
//! - [`BaseAgent`]: the async contract all agents must implement.
//! - [`PAGICoreModel`]: a stub planner that turns a user prompt into a task list.

use async_trait::async_trait;
use interprocess::local_socket::LocalSocketListener;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tracing::{event, Level};

pub mod facts;
pub use facts::{FactType as Fact, FactType, MultimodalFact, RoboticsAction, Vector3D};

// === Authorization / Identity (PoLP) ===

#[derive(Debug, PartialEq, Eq, Clone)]
pub enum AuthScope {
    ReadFacts,
    WriteFacts,
    WritePolicy,
    ExternalAPI,
    RoboticsAction,
}

#[derive(Debug, Clone)]
pub struct AgentIdentity {
    pub id: String,
    pub scopes: Vec<AuthScope>,
}

pub struct AuthorizationGatekeeper;

impl AuthorizationGatekeeper {
    pub fn can_access(identity: &AgentIdentity, required: AuthScope) -> Result<(), String> {
        if identity.scopes.contains(&required) {
            Ok(())
        } else {
            Err(format!(
                "Permission denied: agent '{}' missing required scope {:?}",
                identity.id, required
            ))
        }
    }
}

// Re-export for downstream crates so agents can reopen the DB without declaring a direct
// dependency on `sled`.
pub use sled;

/// Default on-disk knowledge base location (Sled).
pub const KNOWLEDGE_BASE_PATH: &str = "pagi_knowledge_base";

const FACTS_TREE: &str = "facts";

/// A unit of work created by the core planning model.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    /// The agent implementation type to run (e.g., "SearchAgent").
    pub agent_type: String,
    /// JSON payload for the agent.
    pub input_data: String,
}

/// A high-level plan produced by the core planner.
pub type Plan = Vec<Task>;

/// A persistent, structured fact produced by an agent.
#[derive(Debug, Serialize, Deserialize)]
pub struct AgentFact {
    pub agent_id: String,
    pub timestamp: u64,
    pub fact_type: String,
    pub content: String,
}

/// A reflective, self-improvement directive produced by the system.
#[derive(Debug, Serialize, Deserialize)]
pub struct ReflectionFact {
    pub target_agent: String,
    pub critique: String,
    pub new_directive: String,
}

/// A symbolic, rule-based inference rule (IF condition THEN action).
#[derive(Debug, Serialize, Deserialize)]
pub struct PAGIRule {
    pub id: String,
    pub condition_fact_type: String,
    pub condition_keyword: String,
    pub action_directive: String,
}

/// The base contract for all PAGI agents.
///
/// Agents accept an input payload (commonly JSON) and return a structured output string
/// (commonly JSON) after asynchronous processing.
#[async_trait]
pub trait BaseAgent: Send + Sync {
    /// Asynchronously processes the task input and returns a structured result string.
    async fn run(&self, identity: &AgentIdentity, core: Arc<PAGICoreModel>, task_input: &str)
        -> String;
}

/// Default IPC channel name (local socket / pipe).
///
/// The orchestrator initializes a local socket listener using this name, and agents connect
/// to it to send real-time status updates during execution.
///
/// On Unix we use a filesystem-backed socket in `/tmp` so separate processes can discover it.
#[cfg(unix)]
pub const PAGI_IPC_NAME: &str = "/tmp/pagi_shmem_pipe";

/// Default IPC channel name (non-Unix platforms).
#[cfg(not(unix))]
pub const PAGI_IPC_NAME: &str = "pagi_shmem_pipe";

/// The central, shared planning model for the PAGI ecosystem.
///
/// In early scaffolding, this model can be a rule-based stub. Later, it can be replaced
/// with a learned planner or integrated with external reasoning components.
pub struct PAGICoreModel {
    /// IPC listener used for real-time agent status updates.
    ///
    /// This is intentionally an `Option` so the core model can be constructed without
    /// immediately binding a system resource.
    ipc_listener: Option<LocalSocketListener>,

    /// The bound IPC name (may be transformed to a platform-specific path).
    ipc_name: String,

    /// Persistent shared knowledge base (embedded DB).
    knowledge_base: sled::Db,

    /// Tracks whether this model instance successfully initialized the IPC server.
    ///
    /// This prevents non-server instances (e.g., per-agent helper cores) from unlinking the
    /// shared IPC socket path.
    ipc_initialized: bool,

    /// Symbolic rule set used by the inference engine.
    rules: Vec<PAGIRule>,
}

impl Drop for PAGICoreModel {
    fn drop(&mut self) {
        println!("PAGI Core resources are being cleaned up.");

        // Ensure pending KB writes hit disk.
        self.knowledge_base
            .flush()
            .expect("failed to flush knowledge base on drop");

        // Ensure the IPC listener is closed before attempting to unlink the socket path.
        let _ = self.ipc_listener.take();

        // IPC cleanup: only the instance that initialized the IPC server should unlink the
        // socket path. (Per-agent helper core instances should not.)
        #[cfg(unix)]
        if self.ipc_initialized {
            let _ = std::fs::remove_file(&self.ipc_name);
        }
    }
}

impl std::fmt::Debug for PAGICoreModel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PAGICoreModel")
            .field("ipc_name", &self.ipc_name)
            .field("ipc_listener_initialized", &self.ipc_listener.is_some())
            .field("knowledge_base_path", &KNOWLEDGE_BASE_PATH)
            .field("rules_len", &self.rules.len())
            .finish()
    }
}

impl PAGICoreModel {
    #[tracing::instrument(
        level = "trace",
        skip(self, identity),
        fields(identity_id = %identity.id, required_scope = ?scope)
    )]
    pub fn check_authorization(
        &self,
        identity: &AgentIdentity,
        scope: AuthScope,
    ) -> Result<(), String> {
        let res = AuthorizationGatekeeper::can_access(identity, scope.clone());

        if let Err(ref e) = res {
            event!(
                Level::WARN,
                identity_id = %identity.id,
                required_scope = ?scope,
                error = %e,
                "Authorization denied"
            );
        }

        res
    }

    fn default_rules() -> Vec<PAGIRule> {
        vec![
            PAGIRule {
                id: "rule_failure_rerun_deep".to_string(),
                condition_fact_type: "AnalysisResult".to_string(),
                condition_keyword: "Failure".to_string(),
                action_directive: "Rerun: Deep Search".to_string(),
            },
            PAGIRule {
                id: "rule_cyber_alert_triage".to_string(),
                condition_fact_type: "AnalysisResult".to_string(),
                condition_keyword: "CYBER_ALERT".to_string(),
                action_directive: "TASK: CybersecurityAgent, INPUT: Triage alert".to_string(),
            },
        ]
    }

    /// Constructs the core model and opens/creates the persistent knowledge base.
    ///
    /// Note: this follows the prompt's "conceptual stand-in" approach and uses a simple
    /// `unwrap`-style initialization.
    pub fn new() -> Self {
        let db = sled::open(KNOWLEDGE_BASE_PATH).expect("failed to open sled knowledge base");
        Self {
            ipc_listener: None,
            ipc_name: PAGI_IPC_NAME.to_string(),
            knowledge_base: db,
            ipc_initialized: false,
            rules: Self::default_rules(),
        }
    }

    /// Creates a core model from an already-open Sled DB handle.
    ///
    /// Useful for agents that reopen the DB independently (simulating separate processes).
    pub fn from_db(db: sled::Db) -> Self {
        Self {
            ipc_listener: None,
            ipc_name: PAGI_IPC_NAME.to_string(),
            knowledge_base: db,
            ipc_initialized: false,
            rules: Self::default_rules(),
        }
    }

    fn parse_llm_plan(&self, raw: &str) -> Result<Vec<Task>, String> {
        let v: serde_json::Value = serde_json::from_str(raw)
            .map_err(|e| format!("LLM returned non-JSON plan: {e}. Raw: {raw}"))?;
        let arr = v
            .as_array()
            .ok_or_else(|| "LLM plan must be a JSON array".to_string())?;

        let mut tasks = Vec::new();
        for item in arr {
            let agent_type = item
                .get("agent_type")
                .and_then(|v| v.as_str())
                .ok_or_else(|| "Task missing 'agent_type'".to_string())?;

            let input_val = item
                .get("input_data")
                .cloned()
                .unwrap_or_else(|| serde_json::json!({}));

            let input_data = match input_val {
                serde_json::Value::String(s) => s,
                other => other.to_string(),
            };

            tasks.push(Task {
                agent_type: agent_type.to_string(),
                input_data,
            });
        }

        Ok(tasks)
    }

    /// Applies symbolic rules against observed facts and returns action directives.
    pub fn apply_rules_to_facts(&self, facts: Vec<AgentFact>) -> Vec<String> {
        let mut directives = Vec::new();

        for fact in facts {
            for rule in &self.rules {
                if fact.fact_type == rule.condition_fact_type
                    && fact.content.contains(&rule.condition_keyword)
                {
                    directives.push(rule.action_directive.clone());
                }
            }
        }

        directives.sort();
        directives.dedup();
        directives
    }

    fn resolve_symbolic_directives(&self) -> Vec<String> {
        // In a fuller implementation, we'd query a narrower window (e.g., since last run), or
        // only facts produced by specific analysis agents. For now, scan all facts.
        let facts = self.retrieve_facts_by_timestamp_unchecked(0);
        self.apply_rules_to_facts(facts)
    }

    fn apply_symbolic_directives_to_plan(&self, plan: Vec<Task>, directives: &[String]) -> Vec<Task> {
        let wants_deep_rerun = directives
            .iter()
            .any(|d| d.to_lowercase().contains("deep"));

        if !wants_deep_rerun {
            return plan;
        }

        let mut out = Vec::new();

        for task in plan {
            if task.agent_type != "SearchAgent" {
                out.push(task);
                continue;
            }

            // Original task remains.
            out.push(task.clone());

            // Rerun variants (symbolic directive applied).
            for variant in 1..=2u8 {
                let mut payload = serde_json::from_str::<serde_json::Value>(&task.input_data)
                    .unwrap_or_else(|_| serde_json::json!({ "raw": task.input_data }));

                if let serde_json::Value::Object(ref mut m) = payload {
                    m.insert("deep".to_string(), serde_json::Value::Bool(true));
                    m.insert(
                        "rerun_variant".to_string(),
                        serde_json::Value::Number(variant.into()),
                    );
                    m.insert(
                        "symbolic_directives".to_string(),
                        serde_json::Value::Array(
                            directives
                                .iter()
                                .cloned()
                                .map(serde_json::Value::String)
                                .collect(),
                        ),
                    );
                }

                out.push(Task {
                    agent_type: "SearchAgent".to_string(),
                    input_data: payload.to_string(),
                });
            }
        }

        out
    }

    /// Records a fact into the persistent knowledge base.
    #[tracing::instrument(
        level = "trace",
        skip(self, identity, fact),
        fields(
            identity_id = %identity.id,
            agent_id = %fact.agent_id,
            fact_type = %fact.fact_type,
            timestamp = fact.timestamp
        )
    )]
    pub fn record_fact(&self, identity: &AgentIdentity, fact: AgentFact) -> Result<(), String> {
        // Backwards-compatible gating: robotics agents may be granted a narrower scope than
        // full KB writes.
        if self
            .check_authorization(identity, AuthScope::WriteFacts)
            .is_err()
        {
            self.check_authorization(identity, AuthScope::RoboticsAction)?;
        }
        self.record_fact_unchecked(fact)
            .map_err(|e| format!("KB write failed: {e}"))
    }

    fn record_fact_unchecked(&self, fact: AgentFact) -> Result<(), sled::Error> {
        let tree = self.knowledge_base.open_tree(FACTS_TREE)?;
        let id = self.knowledge_base.generate_id()?;

        // Stable, lexicographically sortable key for timestamp queries.
        let key = format!("{:020}_{id}", fact.timestamp);
        let value = serde_json::to_vec(&fact).expect("failed to serialize AgentFact");

        tree.insert(key.as_bytes(), value)?;
        tree.flush()?;
        Ok(())
    }

    /// Retrieves all facts added since the given unix timestamp.
    #[tracing::instrument(
        level = "trace",
        skip(self, identity),
        fields(identity_id = %identity.id, start_ts)
    )]
    pub fn retrieve_facts_by_timestamp(
        &self,
        identity: &AgentIdentity,
        start_ts: u128,
    ) -> Result<Vec<AgentFact>, String> {
        self.check_authorization(identity, AuthScope::ReadFacts)?;

        let facts = self.retrieve_facts_by_timestamp_unchecked(start_ts);
        tracing::event!(Level::DEBUG, facts_len = facts.len(), "KB read completed");
        Ok(facts)
    }

    fn retrieve_facts_by_timestamp_unchecked(&self, start_ts: u128) -> Vec<AgentFact> {
        let start_ts_u64 = u64::try_from(start_ts).unwrap_or(u64::MAX);

        let Ok(tree) = self.knowledge_base.open_tree(FACTS_TREE) else {
            return Vec::new();
        };

        tree.iter()
            .filter_map(|res| res.ok())
            .filter_map(|(k, v)| {
                let key_str = String::from_utf8(k.to_vec()).ok()?;
                let (ts_str, _) = key_str.split_once('_')?;
                let ts = ts_str.parse::<u64>().ok()?;
                if ts < start_ts_u64 {
                    return None;
                }

                serde_json::from_slice::<AgentFact>(&v).ok()
            })
            .collect()
    }

    fn latest_reflection_for_agent(&self, target_agent: &str) -> Option<ReflectionFact> {
        // Reflections are stored as AgentFact entries with fact_type == "ReflectionFact" and
        // JSON-encoded ReflectionFact in `content`.
        let facts = self.retrieve_facts_by_timestamp_unchecked(0);

        facts
            .into_iter()
            .filter(|f| f.fact_type == "ReflectionFact")
            .filter_map(|f| {
                let r = serde_json::from_str::<ReflectionFact>(&f.content).ok()?;
                (r.target_agent == target_agent).then_some((f.timestamp, r))
            })
            .max_by_key(|(ts, _)| *ts)
            .map(|(_, r)| r)
    }

    /// Initializes the IPC server (local socket listener) used for near-real-time status updates.
    ///
    /// The listener is stored internally and can be extracted using [`PAGICoreModel::take_ipc_listener`].
    pub fn init_ipc_server(&mut self) -> Result<(), String> {
        if self.ipc_listener.is_some() {
            return Ok(());
        }

        // Best-effort cleanup on Unix if a prior run left the socket path behind.
        #[cfg(unix)]
        {
            let _ = std::fs::remove_file(PAGI_IPC_NAME);
        }

        let listener = LocalSocketListener::bind(PAGI_IPC_NAME)
            .map_err(|e| format!("Failed to bind IPC server ({}): {e}", PAGI_IPC_NAME))?;

        self.ipc_name = PAGI_IPC_NAME.to_string();
        self.ipc_listener = Some(listener);
        self.ipc_initialized = true;
        Ok(())
    }

    /// Returns the IPC name that agents should connect to.
    pub fn ipc_name(&self) -> &str {
        &self.ipc_name
    }

    /// Takes ownership of the IPC listener so the orchestrator can run an accept/read loop.
    pub fn take_ipc_listener(&mut self) -> Option<LocalSocketListener> {
        self.ipc_listener.take()
    }

    /// Produces a high-level plan from a user prompt.
    ///
    /// For the example prompt:
    /// "Please research the top anti-aging compounds and schedule a team meeting for next week to present the findings."
    /// this method returns two tasks: one for `SearchAgent` and one for `CalendarAgent`.
    #[tracing::instrument(
        level = "trace",
        skip(self, prompt, llm_response_json),
        fields(prompt_len = prompt.len(), llm_response_len = llm_response_json.len())
    )]
    pub async fn general_reasoning(
        &self,
        prompt: &str,
        llm_response_json: &str,
    ) -> Result<Plan, String> {
        // Always keep the fast-path deterministic for security triage.
        let lowered = prompt.to_lowercase();
        if lowered.contains("siem") || lowered.contains("crowdstrike") || lowered.contains("rapid7") {
            return self.general_reasoning_fallback(prompt);
        }

        // If the orchestrator didn't/couldn't provide an LLM response, fall back to the
        // symbolic/rule-based planner.
        if llm_response_json.trim().is_empty()
            || std::env::var("PAGI_DISABLE_LLM").ok().as_deref() == Some("1")
        {
            return self.general_reasoning_fallback(prompt);
        }

        // Parse the LLM plan; if parsing fails, fall back.
        match self.parse_llm_plan(llm_response_json) {
            Ok(tasks) if !tasks.is_empty() => {
                // Symbolic integration: apply symbolic directives over LLM output.
                let directives = self.resolve_symbolic_directives();
                if directives.is_empty() {
                    Ok(tasks)
                } else {
                    Ok(self.apply_symbolic_directives_to_plan(tasks, &directives))
                }
            }
            Ok(_) => self.general_reasoning_fallback(prompt),
            Err(_) => self.general_reasoning_fallback(prompt),
        }
    }

    fn general_reasoning_fallback(&self, prompt: &str) -> Result<Vec<Task>, String> {
        let normalized = prompt.trim();
        let lowered = normalized.to_lowercase();

        // Security-first planning path.
        if lowered.contains("siem") || lowered.contains("crowdstrike") || lowered.contains("rapid7") {
            return Ok(vec![Task {
                agent_type: "CybersecurityAgent".to_string(),
                input_data: serde_json::json!({
                    "action": "Triage alert",
                    "source_prompt": normalized,
                })
                .to_string(),
            }]);
        }

        let example_prompt = "Please research the top anti-aging compounds and schedule a team meeting for next week to present the findings.";

        if normalized == example_prompt {
            // Initial default plan.
            let base_plan = vec![
                Task {
                    agent_type: "SearchAgent".to_string(),
                    input_data: serde_json::json!({
                        "query": "top anti-aging compounds",
                        "deliverable": "summary of leading compounds with citations",
                    })
                    .to_string(),
                },
                Task {
                    agent_type: "CalendarAgent".to_string(),
                    input_data: serde_json::json!({
                        "title": "Anti-aging compounds research review",
                        "timeframe": "next week",
                        "agenda": "Present research findings and next steps",
                    })
                    .to_string(),
                },
            ];

            // Symbolic integration: prioritize symbolic directives over reflection.
            let directives = self.resolve_symbolic_directives();
            if !directives.is_empty() {
                return Ok(self.apply_symbolic_directives_to_plan(base_plan, &directives));
            }

            // Reflection fallback: if no symbolic directive is ready, use reflection facts.
            if let Some(reflection) = self.latest_reflection_for_agent("SearchAgent") {
                let directive = reflection.new_directive.to_lowercase();
                if directive.contains("split") || directive.contains("concurr") {
                    let tasks = vec![
                        Task {
                            agent_type: "SearchAgent".to_string(),
                            input_data: serde_json::json!({
                                "query": "top anti-aging compounds overview",
                                "deliverable": "high-level summary",
                                "directive_applied": reflection.new_directive,
                            })
                            .to_string(),
                        },
                        Task {
                            agent_type: "SearchAgent".to_string(),
                            input_data: serde_json::json!({
                                "query": "anti-aging: rapamycin metformin spermidine",
                                "deliverable": "mechanisms + evidence",
                                "directive_applied": reflection.new_directive,
                            })
                            .to_string(),
                        },
                        Task {
                            agent_type: "SearchAgent".to_string(),
                            input_data: serde_json::json!({
                                "query": "anti-aging: senolytics fisetin quercetin",
                                "deliverable": "senolytic candidates summary",
                                "directive_applied": reflection.new_directive,
                            })
                            .to_string(),
                        },
                        Task {
                            agent_type: "CalendarAgent".to_string(),
                            input_data: serde_json::json!({
                                "title": "Anti-aging compounds research review",
                                "timeframe": "next week",
                                "agenda": "Present research findings and next steps",
                            })
                            .to_string(),
                        },
                    ];
                    return Ok(tasks);
                }
            }

            Ok(base_plan)
        } else {
            Err("No planning rule matched this prompt (stub planner).".to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn example_prompt_returns_two_tasks() {
        let db = sled::Config::new()
            .temporary(true)
            .open()
            .expect("failed to open temporary sled db");
        let model = PAGICoreModel::from_db(db);
        let prompt = "Please research the top anti-aging compounds and schedule a team meeting for next week to present the findings.";
        let tasks = model
            // In tests we pass an empty LLM response so the core uses the deterministic fallback.
            .general_reasoning(prompt, "")
            .await
            .expect("expected Ok plan");

        assert_eq!(tasks.len(), 2);
        assert_eq!(tasks[0].agent_type, "SearchAgent");
        assert_eq!(tasks[1].agent_type, "CalendarAgent");
    }

    #[test]
    fn apply_rules_to_facts_returns_directive_on_match() {
        let db = sled::Config::new()
            .temporary(true)
            .open()
            .expect("failed to open temporary sled db");
        let model = PAGICoreModel::from_db(db);

        let facts = vec![AgentFact {
            agent_id: "ReflectiveAgent".to_string(),
            timestamp: 1,
            fact_type: "AnalysisResult".to_string(),
            content: "Failure: SearchAgent timeout".to_string(),
        }];

        let directives = model.apply_rules_to_facts(facts);
        assert!(directives.iter().any(|d| d.contains("Deep Search")));
    }
}
