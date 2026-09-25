# Diagnostics

Status: implemented as a local-first operator surface. Tidebreak writes no
diagnostic telemetry to a remote service.

Tidebreak keeps bounded measurements and structured events so you can inspect
slow requests, model calls, tool execution, foreground turns, and code turns.
The same administrator-only HTTP routes work in the desktop and self-host
profiles.

## Export diagnostics

In the desktop app, choose **Help → Export Diagnostics…** to save the export
to a file you pick. **Help → Report a Problem…** saves the same file and then
opens a GitHub issue with the app version, operating system, and architecture
filled in, and nothing else; you attach the file yourself. Windows and Linux
builds have no menu bar, so Settings → Updates offers Report a problem… too,
and so do the boot screen and the crash screen. **Help → Show Logs** opens the
profile's `logs` folder.

The desktop saves the export even when its server never started or has
stopped, which is when a report matters most. With no server to ask, the app
builds the bundle itself from the same allowlist. Its snapshot then describes
the app process alone, with no request or model measurements, and the logs,
`boot-failures.log` above all, say what went wrong.

With `TIDEBREAK_DATA_DIR` unset, the command reads the running Tidebreak app:

```sh
tidebreak diagnostics snapshot
tidebreak diagnostics metrics
tidebreak diagnostics export ./tidebreak-diagnostics.zip
```

When the app is not running, the command stops and says so. Add `--embed` to
start an embedded server over the app's data instead. For another profile, set
`TIDEBREAK_DATA_DIR`: the command then starts an embedded server over it, or
add `--attach` when `tidebreak serve` already owns that folder.

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

One more route writes rather than reads. `POST /diagnostics/renderer-errors`
takes an error the renderer could not handle and writes it to the human log;
see [Renderer errors](#renderer-errors).

## Bundle contents

The ZIP contains `snapshot.json`, `metrics.prom`, `manifest.json`, a short
`README.txt`, and available tails from this allowlist:

- `logs/tidebreak.log` and its one rotation.
- `logs/tidebreak.events.jsonl` and its four rotations.
- `boot-failures.log`.

The snapshot contains build and process metadata, uptime, CPU time, maximum
resident memory, Tokio worker and queue gauges, request histograms keyed by
matched route, model duration and token measurements, named operation
histograms, and the health of each background worker. The OpenMetrics file
projects the same measurements for scraping or collector ingestion.

In model usage records, `input_tokens` includes uncached, cache-read, and
cache-creation input. `uncached_input_tokens` keeps the fresh-input component
separate so you can measure cache effectiveness.

The structured log records span-close events and timing summaries. It includes
OpenTelemetry semantic-convention fields for HTTP and generative AI operations
where the local data model has a safe equivalent. Tidebreak does not ship an
OTLP exporter yet. A later exporter can send the same spans without changing
the instrumented call sites.

The request histograms count every HTTP request, but the structured log only
records a request that failed with a 4xx or 5xx status or took 250 ms or
longer. Fast successful requests are mostly local polling, and a record for
each one used to fill the file in minutes. To record a span for every request
while you investigate, see [Logging filters](#logging-filters).

## Background workers

The server runs its long-lived workers under a supervisor: the chat turn
worker, the sandbox workers, blob retirement, the approval judge, the memory
sweep, the MCP supervisor, the gateway model sync, and the rest. When a worker
panics or returns, the supervisor logs it and starts the worker again after a
wait that doubles from one second up to a minute. A worker that stops while
the server shuts down stays stopped.

The snapshot's `workers` list reports each one by name:

```json
{
  "name": "turn_worker",
  "state": "running",
  "restarts": 1,
  "last_error": "the worker panicked: index out of bounds",
  "last_stopped_at": "2026-09-23T10:02:11Z"
}
```

`state` is `running`, `restarting` (waiting to start again), or `stopped`. The
metrics carry the same facts as `tidebreak_worker_up` and
`tidebreak_worker_restarts_total`, labeled by `worker`.

## Panics

Tidebreak records each panic with its message, location, thread, and
backtrace. The desktop, `tidebreak serve`, and every CLI command that writes a
profile's log also append the report to `boot-failures.log` in the profile
data directory. Any other CLI command prints it to stderr. The host broker
sidecar has no log of its own: it writes the report to stderr, which the
desktop forwards into its log, and appends it to the same `boot-failures.log`.

A location that keeps panicking is recorded on its 1st, 2nd, 4th, 8th, and
later powers of two, and only the first carries a backtrace, so a worker that
panics on every restart cannot grow the file without bound.

A panic message is text the log's own emit sites did not choose, so it is
scrubbed before it is written, by the same rules as renderer errors (see
[Scrubbing](#scrubbing)), and cut to 1,000 characters. So is a worker's
`last_error` in the snapshot.

## Renderer errors

The renderer reports three kinds of error: a render error its error boundary
caught, an uncaught exception from the window's `error` event, and a rejected
promise from the window's `unhandledrejection` event. Each report goes to
`POST /diagnostics/renderer-errors`, which writes it to the human log with
every field escaped and cut to a fixed length.

The route takes the same bearer as the rest of the API and sits on the member
plane, because every signed-in renderer reports its own errors. It writes at
most 20 reports at once and then 10 a minute, answering `429` past that. The
renderer adds its own limits: one report for an error that repeats within a
minute, and at most 50 per page load.

Reports never leave the machine. The renderer sends them only to the embedded
server it runs beside, and sends nothing while the window is attached to a
remote machine.

### Scrubbing

The log never carries prompts, URL query strings, credentials, or tokens. Text
from outside the server's own emit sites, such as a renderer error or a panic
message, is scrubbed to keep that rule:

- A URL keeps its scheme, host, and path. Its query string, fragment, and any
  user name or password become `[redacted]`.
- The value after a credential key, such as `token=`, `password:`, or
  `Authorization: Bearer`, becomes `[redacted]`.
- A vendor key, a Tidebreak token, or a JSON web token becomes `[redacted]`
  wherever it appears.

The renderer applies these rules before a report leaves the window, and the
server applies them again before it writes the line. A rejected value that is
not an `Error` is named by its kind and its keys, such as `Object with keys
prompt, url (not an Error)`, and never by its values, so a prompt inside it
does not reach the log.

## After an unclean exit

The desktop writes a marker file at launch and removes it on a clean exit. If
the marker is still there at the next launch, the app quit some other way: a
crash, a force quit, or a power loss. The app then shows a notice, "Tidebreak
quit unexpectedly", with a button that saves this export to a file you pick.
Nothing is sent anywhere; you decide whether to share the file.

An update removes the marker just before it installs. On Windows the
installer ends the process on the spot, without the exit handler, and the
relaunch into the new version would otherwise read as a crash. An install that
fails writes the marker again, because the run goes on.

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
at 10 MiB and keeps four prior files. Each exported log file is capped to the
last 10 MiB of its source. A background thread writes each log file, so a
logging call never waits on the disk. On Unix, Tidebreak writes the log files
and CLI exports with owner-only `0600` permissions.

## Logging filters

`TIDEBREAK_LOG` controls the human log and debug stderr mirror. Diagnostic
timing events stay out of those outputs even if this filter enables their
target. By default, the human log leaves out the warning the database pool
writes for each caller that waits longer than 2 seconds for a connection.
Instead, it writes one summary a minute with the number of slow waits and the
longest one.

`TIDEBREAK_DIAGNOSTICS_LOG` controls the structured JSONL file. Its default
records only the payload-free `tidebreak_diagnostics=info` target. To change
its level, set:

```sh
TIDEBREAK_DIAGNOSTICS_LOG=off,tidebreak_diagnostics=trace
```

At `debug` or finer, the file also records a span for every HTTP request.

Both variables use `tracing-subscriber` filter directives. An invalid value
falls back to the built-in default.
