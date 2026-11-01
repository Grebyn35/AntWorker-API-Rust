// One step of the load test (OPS-18): requests to one replica of the API at a constant rate,
// which `just load-test` runs at rising rates to find the replica's capacity. k6 runs it, and
// prints the step's outcome as one JSON line, which the task reads.
//
// TARGET is the replica's address, RATE the requests a second, DURATION how long the step lasts,
// ROUTE the route the step sends, `capabilities` or `me`, and COLLECTOR the address of the
// collector's own metrics. For `me`, the setup registers a user, and every request carries its
// access token. Before and after the step, the script
// reads how many spans the collector has accepted, so the task can compare them with the requests
// served: a span the API's queue dropped would never reach the collector.

import http from "k6/http";
import { sleep } from "k6";
import { Counter } from "k6/metrics";

const target = __ENV.TARGET;
const rate = Number(__ENV.RATE);
const route = __ENV.ROUTE || "capabilities";
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

// A user's access token, from a registration of an address that no other step used.
function accessToken() {
  const body = {
    email: `load-${Date.now()}-${rate}@example.com`,
    password: "plum-tractor-41-quiet",
    name: "Load Test",
  };
  const response = http.post(`${target}/v1/users`, JSON.stringify(body), {
    headers: { "Content-Type": "application/json" },
    tags: { name: "setup" },
  });
  return response.json("session.access_token");
}

export function setup() {
  const token = route === "me" ? accessToken() : null;
  return { before: acceptedSpans(), token };
}

export default function (data) {
  if (route === "me") {
    http.get(`${target}/v1/users/me`, { headers: { Authorization: `Bearer ${data.token}` } });
  } else {
    http.get(`${target}/v1/capabilities`);
  }
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
