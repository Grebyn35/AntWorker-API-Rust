# Metric catalog

Every metric the dashboards and alerts read: the API's own, from their declarations in code (OBS-11), and those that other parts of the stack produce, from `deploy/observability/metrics.yaml`. `just metrics` writes this file, and `check-metrics` fails while it differs from what the declarations give (OBS-12).

Prometheus stores each metric under the name in its "In Prometheus" column: its receiver of the OpenTelemetry Protocol (OTLP) turns the dots into underscores, and adds the unit and the kind as suffixes. A label's dots become underscores too.

## The API's metrics

| Metric | Kind | Unit | Labels | In Prometheus | Declared in | Description |
|---|---|---|---|---|---|---|
| `http.server.request.duration` | histogram | `s` | `http.request.method`, `http.route`, `http.response.status_code`, `error.type` | `http_server_request_duration_seconds_bucket`, `http_server_request_duration_seconds_sum`, `http_server_request_duration_seconds_count` | `crates/platform/src/http/stack.rs` | How long each request took, from the request log's start to its response, health probes aside. |
| `otel.sdk.exporter.log.exported` | counter | `{log_record}` | `error.type` | `otel_sdk_exporter_log_exported_total` | `crates/platform/src/telemetry/export.rs` | The log records whose export finished, with error.type when it failed. |
| `otel.sdk.exporter.metric_data_point.exported` | counter | `{data_point}` | `error.type` | `otel_sdk_exporter_metric_data_point_exported_total` | `crates/platform/src/telemetry/export.rs` | The metric data points whose export finished, with error.type when it failed. |
| `otel.sdk.exporter.span.exported` | counter | `{span}` | `error.type` | `otel_sdk_exporter_span_exported_total` | `crates/platform/src/telemetry/export.rs` | The spans whose export finished, with error.type when it failed. |
| `platform.database_reachable` | gauge | none | `db.namespace` | `platform_database_reachable` | `crates/platform/src/db/monitor.rs` | 1 while the instance's own database connection answered its last check, 0 otherwise, by the database's name. |
| `platform.errors_ignored` | counter | `{error}` | `error.type` | `platform_errors_ignored_total` | `crates/platform/src/errors.rs` | Errors ignored on purpose, by the name of the failure (ERR-11). |
| `platform.panics` | counter | `{panic}` | none | `platform_panics_total` | `crates/platform/src/telemetry/mod.rs` | Panics, wherever they happened (ERR-12). |
| `process.memory.usage` | gauge | `By` | none | `process_memory_usage_bytes` | `crates/platform/src/telemetry/process.rs` | The physical memory the instance's process uses: its resident set. |

## Metrics of the rest of the stack

| In Prometheus | Kind | Produced by | Description |
|---|---|---|---|
| `up` | gauge | Prometheus | 1 while Prometheus scrapes the target, 0 otherwise. |
| `ALERTS` | gauge | Prometheus | One series for each alert that is pending or firing, with its labels. |
| `system_filesystem_utilization_ratio` | gauge | the collector's host_metrics receiver | The share of a filesystem of the host that is in use, by mountpoint and mode. |
| `system_filesystem_usage_bytes` | gauge | the collector's host_metrics receiver | The bytes of a filesystem of the host, by mountpoint, mode and state, such as free. |
| `system_linux_memory_available_bytes` | gauge | the collector's host_metrics receiver | The memory of the host that is available to processes without swapping. |
| `system_memory_limit_bytes` | gauge | the collector's host_metrics receiver | All the memory of the host. |
| `system_cpu_time_seconds_total` | counter | the collector's host_metrics receiver | The time the host's CPUs spent, by cpu and state, such as idle. |
| `file_mtime_seconds` | gauge | the collector's filestats receiver | When a backup status file was last changed, in seconds since the Unix epoch, by file_name. |
| `otelcol_receiver_refused_spans` | counter | the collector | Spans the collector refused, such as at its memory limit. |
| `otelcol_receiver_refused_log_records` | counter | the collector | Log records the collector refused. |
| `otelcol_receiver_refused_metric_points` | counter | the collector | Metric data points the collector refused. |
| `otelcol_exporter_send_failed_spans` | counter | the collector | Spans the collector failed to send to Tempo. |
| `otelcol_exporter_send_failed_log_records` | counter | the collector | Log records the collector failed to send to Loki. |
| `otelcol_exporter_send_failed_metric_points` | counter | the collector | Metric data points the collector failed to send to Prometheus. |
| `otelcol_processor_tail_sampling_sampling_trace_dropped_too_early` | counter | the collector | Traces tail sampling dropped before deciding them, because it held too many. |
| `otelcol_processor_tail_sampling_sampling_traces_on_memory` | gauge | the collector | The traces tail sampling holds while it waits to decide them. |
| `otelcol_processor_tail_sampling_count_traces_sampled` | counter | the collector | Traces each sampling policy decided, by the policy and whether it kept them. |
