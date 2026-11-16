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

export function teardown() {
  // The replicas export their metrics every few seconds, and Prometheus takes them as they come.
  sleep(15);
  const window = `${Math.ceil(parseFloat(duration)) + 15}s`;
  const query =
    "1000 * histogram_quantile(0.99, sum by (le) (" +
    `increase(platform_event_delivery_lag_seconds_bucket[${window}]) ` +
    "and on (instance) (platform_streams > 0)))";
  const response = http.get(`${prometheus}/api/v1/query?query=${encodeURIComponent(query)}`, {
    tags: { name: "prometheus" },
  });
  const result = response.json("data.result");
  if (result && result.length > 0) {
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
