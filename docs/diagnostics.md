# Diagnostics

Status: implemented as a local-first operator surface. Tidebreak writes no
diagnostic telemetry to a remote service.

Tidebreak keeps bounded measurements and structured events so you can inspect
slow requests, model calls, tool execution, foreground turns, and code turns.
The same administrator-only HTTP routes work in the desktop and self-host
profiles.

## Export diagnostics

If the desktop or `tidebreak serve` owns the profile, attach to that process:

```sh
tidebreak diagnostics snapshot --attach
tidebreak diagnostics metrics --attach
tidebreak diagnostics export ./tidebreak-diagnostics.zip --attach
```

Without `--attach` or `--server`, the command starts an embedded server for the
selected profile. A running desktop already holds that profile's lock, so use
`--attach` for the desktop process.

To inspect a self-host server, pass its URL and an administrator bearer token:

```sh
TIDEBREAK_SERVER_TOKEN=<token> \
  tidebreak diagnostics export ./tidebreak-diagnostics.zip \
  --server https://tidebreak.example.com
```

The routes are:

- `GET /diagnostics/snapshot` returns JSON.
- `GET /diagnostics/metrics` returns OpenMetrics text.
- `GET /diagnostics/export` returns a ZIP archive.

The desktop launch token resolves to the local owner. On self-host, members
receive `403 Forbidden`; only administrators can read or export diagnostics.

## Bundle contents

The ZIP contains `snapshot.json`, `metrics.prom`, `manifest.json`, a short
`README.txt`, and available tails from this allowlist:

- `logs/tidebreak.log` and its one rotation.
- `logs/tidebreak.events.jsonl` and its one rotation.
- `boot-failures.log`.

The snapshot contains build and process metadata, uptime, CPU time, maximum
resident memory, Tokio worker and queue gauges, request histograms keyed by
matched route, model duration and token measurements, and named operation
histograms. The OpenMetrics file projects the same measurements for scraping or
collector ingestion.

In model usage records, `input_tokens` includes uncached, cache-read, and
cache-creation input. `uncached_input_tokens` keeps the fresh-input component
separate so you can measure cache effectiveness.

The structured log records span-close events and timing summaries. It includes
OpenTelemetry semantic-convention fields for HTTP and generative AI operations
where the local data model has a safe equivalent. Tidebreak does not ship an
OTLP exporter yet. A later exporter can send the same spans without changing
the instrumented call sites.

## Privacy and bounds

The export code reads only the allowlist above. It does not read the database,
conversation transcripts, blobs, attachments, credentials, keychain values, or
arbitrary files from the profile directory.

HTTP measurements use the matched route pattern, such as `/chats/{id}`. They
never record the raw URL or query string. Model and tool spans record names,
counts, durations, outcomes, and token usage. They do not record prompts, model
output, tool arguments, or tool results.

Logs can still contain local paths, opaque record IDs, and bounded provider
diagnostics emitted elsewhere in Tidebreak. Review an archive before sharing
it.

The human log rotates at 5 MiB and keeps one prior file. The JSONL log rotates
at 10 MiB and also keeps one prior file. Each exported log file is capped to
the last 10 MiB of its source. On Unix, Tidebreak writes the log files and CLI
exports with owner-only `0600` permissions.

## Logging filters

`TIDEBREAK_LOG` controls the human log and debug stderr mirror. Diagnostic
timing events stay out of those outputs even if this filter enables their
target.

`TIDEBREAK_DIAGNOSTICS_LOG` controls the structured JSONL file. Its default
records only the payload-free `tidebreak_diagnostics=info` target. To change
its level, set:

```sh
TIDEBREAK_DIAGNOSTICS_LOG=off,tidebreak_diagnostics=trace
```

Both variables use `tracing-subscriber` filter directives. An invalid value
falls back to the built-in default.
