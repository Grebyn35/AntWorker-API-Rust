// One step of the load test (OPS-16): requests to one replica of the API at a constant rate,
// which `just load-test` runs at rising rates to find the replica's capacity. k6 runs it, and
// prints the step's outcome as one JSON line, which the task reads.
//
// TARGET is the replica's address, RATE the requests a second, DURATION how long the step lasts,
// ROUTE the route the step sends, `capabilities`, `me` or `sign-ins`, and COLLECTOR the address
// of the collector's own metrics. For `me`, the setup registers a user, and every request carries
// its access token. For `sign-ins`, the setup registers 20 users, each request signs one of them
// in with their password, and beside them the step reads the capabilities 50 times a second,
// the other requests, whose own outcome the step reports: a sign-in hashes a password, the
// API's CPU-heavy work, and the step shows what the others wait for it. Before and after the
// step, the script reads how many spans the collector has accepted, so the task can compare them
// with the requests served: a span the API's queue dropped would never reach the collector.

import http from "k6/http";
import { fail, sleep } from "k6";
import { Counter } from "k6/metrics";

const target = __ENV.TARGET;
const rate = Number(__ENV.RATE);
const route = __ENV.ROUTE || "capabilities";
const collector = __ENV.COLLECTOR;
const duration = __ENV.DURATION || "30s";

// The other requests that a step of sign-ins sends beside them, each second.
const OTHERS_RATE = 50;
// The users that a step of sign-ins signs in, in turn.
const USERS = 20;
const PASSWORD = "plum-tractor-41-quiet";

const scenarios = {
  capacity: {
    executor: "constant-arrival-rate",
    rate,
    timeUnit: "1s",
    duration,
    preAllocatedVUs: Math.max(20, Math.ceil(rate / 5)),
    maxVUs: Math.max(200, rate * 2),
  },
};
if (route === "sign-ins") {
  scenarios.others = {
    executor: "constant-arrival-rate",
    exec: "other",
    rate: OTHERS_RATE,
    timeUnit: "1s",
    duration,
    preAllocatedVUs: 20,
    maxVUs: 200,
  };
}

// Thresholds that always pass make k6 report the metrics of each scenario's own requests apart
// from the setup's and the two that read the collector.
const thresholds = {};
for (const scenario of Object.keys(scenarios)) {
  thresholds[`http_reqs{scenario:${scenario}}`] = ["count>=0"];
  thresholds[`http_req_failed{scenario:${scenario}}`] = ["rate>=0"];
  thresholds[`http_req_duration{scenario:${scenario}}`] = ["p(99)>=0"];
}

export const options = {
  scenarios,
  thresholds,
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

const json = { headers: { "Content-Type": "application/json" } };

// Registers `email`, an address no other step used, and returns the access token of the session
// that the registration answers (AUTH-001). Without one, the step fails at once, rather than
// measuring 401s.
function register(email) {
  const body = { email, password: PASSWORD, name: "Load Test" };
  const response = http.post(`${target}/v1/users`, JSON.stringify(body), {
    headers: json.headers,
    tags: { name: "setup" },
  });
  const token = response.json("access_token");
  if (typeof token !== "string") {
    fail(`registering the load test's user answered ${response.status} without an access token`);
  }
  return token;
}

export function setup() {
  const stamp = `${Date.now()}-${rate}`;
  const token = route === "me" ? register(`load-${stamp}@example.com`) : null;
  const emails = [];
  if (route === "sign-ins") {
    for (let user = 0; user < USERS; user++) {
      const email = `load-${stamp}-${user}@example.com`;
      register(email);
      emails.push(email);
    }
  }
  return { before: acceptedSpans(), token, emails };
}

export default function (data) {
  if (route === "me") {
    http.get(`${target}/v1/users/me`, { headers: { Authorization: `Bearer ${data.token}` } });
  } else if (route === "sign-ins") {
    const email = data.emails[__ITER % data.emails.length];
    http.post(`${target}/v1/sessions`, JSON.stringify({ email, password: PASSWORD }), json);
  } else {
    http.get(`${target}/v1/capabilities`);
  }
}

// One of the other requests that a step of sign-ins sends beside them.
export function other() {
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
        others: metric("http_reqs{scenario:others}").count || 0,
        others_failed: metric("http_req_failed{scenario:others}").passes || 0,
        others_p99_ms: metric("http_req_duration{scenario:others}")["p(99)"] || 0,
        spans: metric("collector_accepted_spans").count || 0,
      }) + "\n",
  };
}
