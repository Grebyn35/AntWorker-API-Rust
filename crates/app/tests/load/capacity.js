// One step of the load test (OPS-18): requests to one replica of the API at a constant rate,
// which `just load-test` runs at rising rates to find the replica's capacity. k6 runs it, and
// prints the step's outcome as one JSON line, which the task reads.
//
// TARGET is the replica's address, RATE the requests a second, DURATION how long the step lasts,
// and COLLECTOR the address of the collector's own metrics. Before and after the step, the script
// reads how many spans the collector has accepted, so the task can compare them with the requests
// served: a span the API's queue dropped would never reach the collector.

import http from "k6/http";
import { sleep } from "k6";
import { Counter } from "k6/metrics";

const target = __ENV.TARGET;
const rate = Number(__ENV.RATE);
const collector = __ENV.COLLECTOR;

export const options = {
  scenarios: {
    capacity: {
      executor: "constant-arrival-rate",
      rate,
      timeUnit: "1s",
      duration: __ENV.DURATION || "30s",
      preAllocatedVUs: Math.max(20, Math.ceil(rate / 5)),
      maxVUs: Math.max(200, rate * 2),
    },
  },
  // Thresholds that always pass make k6 report the metrics of the step's own requests apart
  // from the two that read the collector.
  thresholds: {
    "http_reqs{scenario:capacity}": ["count>=0"],
    "http_req_failed{scenario:capacity}": ["rate>=0"],
    "http_req_duration{scenario:capacity}": ["p(99)>=0"],
  },
  summaryTrendStats: ["p(99)", "max"],
};

const accepted = new Counter("collector_accepted_spans");

// The spans the collector has accepted from the API so far.
function acceptedSpans() {
  const response = http.get(`${collector}/metrics`, { tags: { name: "collector" } });
  let total = 0;
  for (const line of response.body.split("\n")) {
    if (line.startsWith("otelcol_receiver_accepted_spans")) {
      total += Number(line.split(" ").pop());
    }
  }
  return total;
}

export function setup() {
  return { before: acceptedSpans() };
}

export default function () {
  http.get(`${target}/v1/capabilities`);
}

export function teardown(data) {
  // The API exports its spans at least once a second, and the collector counts them when it
  // receives them.
  sleep(5);
  accepted.add(acceptedSpans() - data.before);
}

export function handleSummary(data) {
  const metric = (name) => (data.metrics[name] ? data.metrics[name].values : {});
  return {
    stdout:
      JSON.stringify({
        rate,
        requests: metric("http_reqs{scenario:capacity}").count || 0,
        failed: metric("http_req_failed{scenario:capacity}").passes || 0,
        dropped: metric("dropped_iterations").count || 0,
        p99_ms: metric("http_req_duration{scenario:capacity}")["p(99)"] || 0,
        spans: metric("collector_accepted_spans").count || 0,
      }) + "\n",
  };
}
