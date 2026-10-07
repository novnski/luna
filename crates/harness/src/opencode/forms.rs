//! Questions: mapping native question requests onto the input panel.

use super::*;

/// `question.asked` → the input panel's questions (ids are positional).
pub(super) fn map_questions(props: &Value) -> Vec<UserInputQuestion> {
    props
        .get("questions")
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .enumerate()
                .filter_map(|(ix, q)| {
                    let question = q.get("question").and_then(Value::as_str)?;
                    Some(UserInputQuestion {
                        id: format!("q{ix}"),
                        header: q
                            .get("header")
                            .and_then(Value::as_str)
                            .unwrap_or("Question")
                            .to_owned(),
                        question: question.to_owned(),
                        options: q
                            .get("options")
                            .and_then(Value::as_array)
                            .map(|opts| {
                                opts.iter()
                                    .filter_map(|o| o.get("label").and_then(Value::as_str))
                                    .map(str::to_owned)
                                    .collect()
                            })
                            .unwrap_or_default(),
                        multi_select: q.get("multiple").and_then(Value::as_bool).unwrap_or(false),
                        mcp: None,
                        approval: None,
                        input: None,
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// A native `question.asked` request, with what answering it needs.
pub(super) struct QuestionAsk<'a> {
    pub(super) props: &'a Value,
    pub(super) event_session: Option<&'a str>,
    pub(super) session_id: &'a str,
    pub(super) children: &'a HashMap<String, ChildRun>,
    pub(super) unbound_children: &'a HashMap<String, String>,
    pub(super) server: &'a Server,
    pub(super) dir: Option<&'a str>,
    pub(super) event_tx: &'a mpsc::Sender<Result<AgentEvent, HarnessError>>,
    pub(super) request_input: &'a Arc<RequestInput>,
}

/// Route a question owned by this run (or one of its live children) to
/// the input panel and reply on the native route.
pub(super) async fn handle_question_asked(ask: QuestionAsk<'_>) -> BusOutcome {
    let QuestionAsk {
        props,
        event_session,
        session_id,
        children,
        unbound_children,
        server,
        dir,
        event_tx,
        request_input,
    } = ask;
    if !event_session.is_some_and(|session| {
        session == session_id
            || children.get(session).is_some_and(|child| !child.done)
            || unbound_children.contains_key(session)
    }) {
        return BusOutcome::Continue;
    }
    let Some(id) = props.get("id").and_then(Value::as_str) else {
        return BusOutcome::Continue;
    };
    let questions = map_questions(props);
    if questions.is_empty() {
        return BusOutcome::Continue;
    }
    if !send(
        event_tx,
        AgentEvent::InputRequested {
            request_id: id.to_owned(),
            questions: questions.clone(),
        },
    )
    .await
    {
        return BusOutcome::ConsumerGone;
    }
    let rx = (request_input)(questions.clone());
    let base = server.base.clone();
    let auth = server.auth.clone();
    let dir_owned = dir.map(str::to_owned);
    let request_id = id.to_owned();
    let tx = event_tx.clone();
    let protocol_cell = server.protocol.clone();
    tokio::spawn(async move {
        let server = Server {
            child: None,
            base,
            auth,
            client: http_client(),
            stderr_tail: crate::StderrTail::default(),
            protocol: protocol_cell,
        };
        let reply = match rx.await {
            Ok(answers) => {
                let ordered: Vec<Vec<String>> = questions
                    .iter()
                    .map(|q| {
                        answers
                            .iter()
                            .find(|a| a.question_id == q.id)
                            .map(|a| a.labels.clone())
                            .unwrap_or_default()
                    })
                    .collect();
                server
                    .post_json(
                        &format!("/question/{request_id}/reply"),
                        dir_owned.as_deref(),
                        &json!({ "answers": ordered }),
                    )
                    .await
            }
            Err(_) => {
                server
                    .post_json(
                        &format!("/question/{request_id}/reject"),
                        dir_owned.as_deref(),
                        &Value::Null,
                    )
                    .await
            }
        };
        if let Err(e) = reply {
            tracing::debug!(
                target: "luna_harness::opencode",
                "question reply failed: {e}"
            );
        }
        let _ = tx
            .send(Ok(AgentEvent::InputResolved {
                request_id: request_id.clone(),
            }))
            .await;
    });
    BusOutcome::Continue
}
