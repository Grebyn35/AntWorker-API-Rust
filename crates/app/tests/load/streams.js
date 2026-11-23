// One step of the hub's load test (OPS-16): `just load-test streams` runs it at rising rates. It
// publishes events at a constant rate through the publishing replica, by renaming the workspace of
// each stream's user, its owner and only member, as that user, which publishes `workspace.updated`
// to the user's streams on the measured replica (WORKSPACES-008): an event that changes no one's
// access, so no stream closes. k6 runs it, and prints the step's outcome as one JSON line, which
// the task reads.
//
// TARGET is the publishing replica's address, RATE the renames a second, DURATION how long the
// step lasts, DATA the file of the workspaces and their owners' access tokens, which the task
// wrote, and PROMETHEUS the address of the Prometheus that the replicas' metrics reach. After the
// step, the script reads from Prometheus the 99th percentile of the lag of the events that the
// measured replica's hub delivered: the replica whose hub holds streams.

import http from "k6/http";
import exec from "k6/execution";
import { sleep } from "k6";
import { Trend } from "k6/metrics";

const target = __ENV.TARGET;
const rate = Number(__ENV.RATE);
const duration = __ENV.DURATION || "30s";
const prometheus = __ENV.PROMETHEUS;
const data = JSON.parse(open(__ENV.DATA));

export const options = {
  scenarios: {
    streams: {
      executor: "constant-arrival-rate",
      rate,
      timeUnit: "1s",
      duration,
      preAllocatedVUs: Math.max(20, Math.ceil(rate / 5)),
      maxVUs: Math.max(200, rate * 2),
    },
  },
  // Thresholds that always pass make k6 report the metrics of the step's own requests apart from
  // the one that reads Prometheus.
  thresholds: {
    "http_reqs{scenario:streams}": ["count>=0"],
    "http_req_failed{scenario:streams}": ["rate>=0"],
    "http_req_duration{scenario:streams}": ["p(99)>=0"],
  },
  summaryTrendStats: ["p(99)", "max"],
};

const lag = new Trend("hub_lag_p99_ms");

// How long the teardown waits for Prometheus to hold a sample of the measured hub's lag that the
// replica took after the step's requests ended: the replicas export their metrics every 15
// seconds (the platform spec's Telemetry export, under Batching), so this is four of their
// exports. A step whose sample doesn't come by then reports no lag, and fails.
const SAMPLED_MS = 60000;

// What keeps the series of the measured hub, the replica whose hub holds streams.
const MEASURED = "and on (instance) (platform_streams > 0)";

// The series that Prometheus answers to the query `text`, or none.
function query(text) {
  const response = http.get(`${prometheus}/api/v1/query?query=${encodeURIComponent(text)}`, {
    tags: { name: "prometheus" },
  });
  return response.json("data.result") || [];
}

export function setup() {
  return { started: Date.now() };
}

export default function () {
  const iteration = exec.scenario.iterationInTest;
  const workspace = data.workspaces[iteration % data.workspaces.length];
  http.patch(
    `${target}/v1/workspaces/${workspace.id}`,
    JSON.stringify({ name: `Load Test ${iteration}` }),
    {
      headers: {
        "Content-Type": "application/merge-patch+json",
        Authorization: `Bearer ${workspace.token}`,
      },
    },
  );
}

// Waits until Prometheus's newest sample of the measured hub's lag, by its timestamp, is one
// that the replica took after the step's last request ended, which holds the lag of each event
// that the hub loaded before the replica took it, or until SAMPLED_MS has passed. Then it reads
// the 99th percentile of the lags that the samples since the step began hold.
export function teardown(step) {
  const ended = Date.now();
  // timestamp() gives a sample's own time only of a series that it reads directly.
  const newest = () => {
    const result = query(
      `max(timestamp(platform_event_delivery_lag_seconds_count) ${MEASURED})`,
    );
    return result.length > 0 ? 1000 * Number(result[0].value[1]) : 0;
  };
  while (newest() <= ended) {
    if (Date.now() - ended > SAMPLED_MS) {
      return;
    }
    sleep(1);
  }
  const window = `${Math.ceil((Date.now() - step.started) / 1000)}s`;
  const result = query(
    "1000 * histogram_quantile(0.99, sum by (le) (" +
      `increase(platform_event_delivery_lag_seconds_bucket[${window}]) ${MEASURED}))`,
  );
  if (result.length > 0) {
    lag.add(Number(result[0].value[1]));
  }
}

export function handleSummary(summary) {
  const metric = (name) => (summary.metrics[name] ? summary.metrics[name].values : {});
  const p99 = metric("hub_lag_p99_ms")["p(99)"];
  return {
    stdout:
      JSON.stringify({
        rate,
        requests: metric("http_reqs{scenario:streams}").count || 0,
        failed: metric("http_req_failed{scenario:streams}").passes || 0,
        dropped: metric("dropped_iterations").count || 0,
        p99_ms: metric("http_req_duration{scenario:streams}")["p(99)"] || 0,
        lag_p99_ms: Number.isFinite(p99) ? p99 : null,
      }) + "\n",
  };
}
