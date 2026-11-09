# Grafana LGTM

Grafana, Tempo, Loki, Prometheus and an OpenTelemetry Collector in one container, `grafana/otel-lgtm:0.11.16`.
Parity uses the same image and version for its own local tracing.
This example adds a `polkadot-frame-trace` dashboard.

| Port | What |
| --- | --- |
| 3000 | Grafana |
| 4318 | OTLP/HTTP, where polkadot-frame-trace sends traces |
| 4317 | OTLP/gRPC, for other tools |

| File | What |
| --- | --- |
| `docker-compose.yml` | The container, with the dashboard mounted. |
| `provisioning/polkadot-frame-trace.yaml` | Tells Grafana to load the dashboard. |
| `dashboards/polkadot-frame-trace.json` | The dashboard. |

[Send traces to Grafana](../../docs/guides/send-traces.md#grafana) shows how to start it, send a trace and open the dashboard.
[Find storage problems with TraceQL](../../docs/guides/query-traces-in-grafana.md) has ready queries.
