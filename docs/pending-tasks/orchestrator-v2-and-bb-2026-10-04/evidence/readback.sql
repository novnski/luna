.bail on
SELECT thread_id, provider_thread_id, driver, status,
       json_extract(payload_json,'$.nativeThreadRef') AS nativeThreadRef,
       json_extract(payload_json,'$.forkedFrom') AS forkedFrom
FROM orchestration_v2_projection_provider_threads
WHERE thread_id IN ('ecce5cc4-8416-4a84-aaf3-9e241fb13d91','18fa694d-dcd2-46f2-865f-ff02ac99b309');

SELECT type, status, source_thread_id, target_thread_id
FROM orchestration_v2_projection_context_transfers
WHERE source_thread_id IN ('ecce5cc4-8416-4a84-aaf3-9e241fb13d91','18fa694d-dcd2-46f2-865f-ff02ac99b309');

SELECT strategy, status,
       json_extract(payload_json,'$.delivery.status') AS deliveryStatus,
       json_array_length(payload_json,'$.history.messages') AS transferredMessages,
       json_extract(payload_json,'$.history.omittedItems') AS omittedItems
FROM orchestration_v2_projection_context_handoffs
WHERE thread_id IN ('ecce5cc4-8416-4a84-aaf3-9e241fb13d91','18fa694d-dcd2-46f2-865f-ff02ac99b309');

SELECT thread_id, run_id, status,
       json_extract(payload_json,'$.ordinal') AS ordinal,
       json_extract(payload_json,'$.providerInstanceId') AS provider
FROM orchestration_v2_projection_runs
WHERE thread_id IN ('ecce5cc4-8416-4a84-aaf3-9e241fb13d91','18fa694d-dcd2-46f2-865f-ff02ac99b309')
ORDER BY thread_id, ordinal;
