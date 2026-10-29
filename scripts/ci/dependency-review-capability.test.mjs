import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { runInNewContext } from "node:vm";

const workflow = readFileSync(new URL("../../.github/workflows/_security.yml", import.meta.url), "utf8");

// Read only the known job/step indentation rather than depending on a YAML package.
function job(name) {
  const marker = `\n  ${name}:\n`;
  assert.equal(workflow.split(marker).length, 2, `Expected exactly one ${name} job`);
  return workflow.split(marker)[1].split(/\n  [a-z][a-z0-9-]*:\n/u)[0];
}

function block(step, key, indentation) {
  const marker = `${" ".repeat(indentation)}${key}: |\n`;
  assert.equal(step.split(marker).length, 2, `Expected exactly one inline ${key} block`);
  const lines = step.split(marker)[1].trimEnd().split("\n");
  const prefix = " ".repeat(indentation + 2);
  assert.ok(lines.every((line) => line === "" || line.startsWith(prefix)), "Unexpected block indentation");
  return lines.map((line) => line.slice(prefix.length)).join("\n");
}

const reviewJob = job("dependency-review");
const steps = reviewJob.split(/^      - /mu).slice(1);
const prerequisite = steps.find((step) => step.startsWith("name: Require the full npm and Rust audit matrix\n"));
const capability = steps.find((step) => step.startsWith("name: Check dependency review capability\n"));
const review = steps.find((step) => step.startsWith("uses: actions/dependency-review-action@"));
assert.ok(prerequisite && capability && review, "Required dependency-review steps must exist");
const AsyncFunction = Object.getPrototypeOf(async function () {}).constructor;
const evaluateCapability = new AsyncFunction("github", "context", "core", block(capability, "script", 10));
const prerequisiteScript = block(prerequisite, "run", 8);
const prerequisiteCondition = prerequisite.match(/^        if: (.+)$/mu)?.[1];
assert.ok(prerequisiteCondition, "The full-audit prerequisite must have a condition");

const docs = "https://docs.github.com/rest/dependency-graph/dependency-review#get-a-diff-of-the-dependencies-between-commits";
function apiError({ status = 403, message = "Forbidden", headers = {}, documentationUrl = docs } = {}) {
  return Object.assign(new Error(message), {
    status,
    response: { data: { message, documentation_url: documentationUrl }, headers },
  });
}

function fixture(options = {}) {
  const reviewError = Object.hasOwn(options, "reviewError") ? options.reviewError : apiError();
  const calls = [];
  const outputs = [];
  const warnings = [];
  const summary = [];
  const context = {
    repo: { owner: "base-owner", repo: "base-repo" },
    serverUrl: "https://github.com",
    payload: {
      pull_request: {
        base: { sha: "base-sha" },
        head: { sha: "head-sha", repo: { owner: { login: "fork-owner" }, name: "fork-repo" } },
      },
    },
  };
  const comparison = { ...context.repo, basehead: "base-sha...head-sha", per_page: 1 };
  const core = {
    setOutput: (key, value) => outputs.push([key, value]),
    warning: (message) => warnings.push(message),
    summary: {
      addHeading(heading) { summary.push(["heading", heading]); return this; },
      addRaw(message) { summary.push(["message", message]); return this; },
      async write() { summary.push(["write"]); },
    },
  };
  const github = {
    async request(route, args) {
      calls.push("review");
      assert.equal(route, "GET /repos/{owner}/{repo}/dependency-graph/compare/{basehead}");
      assert.deepEqual(args, comparison);
      if (reviewError) throw reviewError;
      return { data: [] };
    },
    rest: { repos: {
      async get(args) {
        calls.push("metadata");
        assert.deepEqual(args, context.repo);
        if (options.metadataError) throw options.metadataError;
        return { data: options.repository ?? { private: false, visibility: "public" } };
      },
      async compareCommitsWithBasehead(args) {
        calls.push("revisions");
        assert.deepEqual(args, comparison);
        if (options.revisionsError) throw options.revisionsError;
        return { data: {} };
      },
    } },
    async graphql(query, variables) {
      calls.push("graph");
      assert.deepEqual(variables, context.repo);
      // This repository-reading query must not grow admin, mutation, or alert-access requirements.
      assert.equal(query.replace(/\s+/gu, " ").trim(),
        "query($owner: String!, $repo: String!) { repository(owner: $owner, name: $repo) { dependencyGraphManifests(first: 1) { totalCount } } }");
      if (options.graphError) throw options.graphError;
      return Object.hasOwn(options, "graph") ? options.graph : {
        repository: { dependencyGraphManifests: { totalCount: 0 } },
      };
    },
  };
  return {
    calls, outputs, warnings, summary, reviewError,
    run: () => evaluateCapability(github, context, core),
  };
}

test("supported repositories retain dependency review without probing fallback APIs", async () => {
  const f = fixture({ reviewError: null });
  await f.run();
  assert.deepEqual(f.calls, ["review"]);
  assert.deepEqual(f.outputs, [["supported", "true"]]);
  assert.deepEqual(f.warnings, []);
  assert.deepEqual(f.summary, []);
  assert.match(review, /^        if: steps\.capability\.outputs\.supported == 'true'$/mu);
  assert.match(review, /^          fail-on-severity: high$/mu);
  assert.match(review, /^          comment-summary-in-pr: never$/mu);
});

test("the observed public 403 with readable revisions and zero manifests warns and uses full audits", async () => {
  const f = fixture();
  await f.run();
  assert.deepEqual(f.calls, ["review", "metadata", "revisions", "graph"]);
  assert.deepEqual(f.outputs, [["supported", "false"]]);
  assert.equal(f.warnings.length, 1);
  assert.match(f.warnings[0], /Enable the free Dependency graph/u);
  assert.ok(f.warnings[0].includes("https://github.com/base-owner/base-repo/settings/security_analysis"));
  assert.match(f.warnings[0], /full npm and Rust dependency-security audits passed and remain required/u);
  assert.match(f.warnings[0], /PR dependency-diff review was not performed/u);
  assert.deepEqual(f.summary, [
    ["heading", "Dependency review fallback"], ["message", f.warnings[0]], ["write"],
  ]);
});

const failureCases = [
  { name: "private repository", options: { repository: { private: true, visibility: "private" } }, calls: ["review", "metadata"] },
  { name: "internal repository", options: { repository: { private: true, visibility: "internal" } }, calls: ["review", "metadata"] },
  { name: "missing visibility", options: { repository: { private: false } }, calls: ["review", "metadata"] },
  { name: "missing private flag", options: { repository: { visibility: "public" } }, calls: ["review", "metadata"] },
  ...[401, 404, 429, 500, 503].map((status) => ({
    name: `HTTP ${status}`, options: { reviewError: apiError({ status }) }, calls: ["review"],
  })),
  { name: "integration permission denial", options: { reviewError: apiError({ message: "Resource not accessible by integration" }) }, calls: ["review"] },
  { name: "secondary rate limit", options: { reviewError: apiError({ message: "You have exceeded a secondary rate limit." }) }, calls: ["review"] },
  { name: "exhausted rate budget", options: { reviewError: apiError({ headers: { "x-ratelimit-remaining": "0" } }) }, calls: ["review"] },
  { name: "retry-after", options: { reviewError: apiError({ headers: { "retry-after": "60" } }) }, calls: ["review"] },
  { name: "unexpected API documentation", options: { reviewError: apiError({ documentationUrl: "https://docs.github.com/rest" }) }, calls: ["review"] },
  { name: "missing API documentation", options: { reviewError: apiError({ documentationUrl: null }) }, calls: ["review"] },
  { name: "network failure", options: { reviewError: new Error("Network unavailable") }, calls: ["review"] },
  { name: "repository metadata failure", options: { metadataError: apiError({ status: 500 }) }, calls: ["review", "metadata"] },
  { name: "inaccessible PR revisions", options: { revisionsError: apiError({ status: 404 }) }, calls: ["review", "metadata", "revisions"] },
  { name: "GraphQL permission denial", options: { graphError: new Error("Resource not accessible by integration") }, calls: ["review", "metadata", "revisions", "graph"] },
  { name: "GraphQL service failure", options: { graphError: apiError({ status: 503 }) }, calls: ["review", "metadata", "revisions", "graph"] },
  ...[
    { repository: { dependencyGraphManifests: { totalCount: 39 } } },
    { repository: null },
    { repository: {} },
    { repository: { dependencyGraphManifests: null } },
    { repository: { dependencyGraphManifests: {} } },
    { repository: { dependencyGraphManifests: { totalCount: "0" } } },
  ].map((graph, index) => ({
    name: `existing or invalid graph metadata ${index + 1}`, options: { graph }, calls: ["review", "metadata", "revisions", "graph"],
  })),
];

for (const { name, options, calls } of failureCases) {
  test(`fails closed: ${name}`, async () => {
    const f = fixture(options);
    const expected = options.metadataError ?? options.revisionsError ?? options.graphError ?? f.reviewError;
    await assert.rejects(f.run, (error) => error === expected);
    assert.deepEqual(f.calls, calls);
    assert.deepEqual(f.outputs, []);
    assert.deepEqual(f.warnings, []);
    assert.deepEqual(f.summary, []);
  });
}

test("the full npm/Rust audit matrix is an unconditional prerequisite, not a skipped success", () => {
  assert.match(reviewJob, /^    needs: dependency-security$/mu);
  assert.match(reviewJob, /^    if: always\(\) && inputs\.dependency-review && github\.event_name == 'pull_request'$/mu);
  assert.ok(steps.indexOf(prerequisite) < steps.indexOf(capability));
  assert.ok(steps.indexOf(capability) < steps.indexOf(review));
  assert.match(prerequisiteScript, /::error::All dependency-security audits must pass/u);
  assert.match(prerequisiteScript, /^exit 1$/mu);
  assert.doesNotMatch(reviewJob, /continue-on-error/u);

  const audits = job("dependency-security");
  assert.doesNotMatch(audits, /^    if:|continue-on-error/mu);
  assert.match(audits, /^      fail-fast: false$/mu);
  assert.deepEqual([...audits.matchAll(/^          - ecosystem: (.+)$/gmu)].map((match) => match[1]), [
    "npm", "rust-network-contracts", "rust-network-agent", "rust-state-agent", "rust-desktop", "rust-mic-sidecar", "rust-mic-receiver",
  ]);
  assert.match(audits, /^        run: npm audit --audit-level=high$/mu);
  assert.match(audits, /cargo audit --file "\$\{\{ matrix\.directory \}\}\/Cargo\.lock"/u);
});

// Evaluate the actual Actions condition after mapping its hyphenated identifier to a JS binding.
for (const result of ["success", "failure", "cancelled", "skipped"]) {
  test(`full audit prerequisite ${result === "success" ? "allows" : "blocks"} matrix result: ${result}`, () => {
    const expression = prerequisiteCondition.replaceAll("needs.dependency-security.result", "auditResult");
    assert.equal(runInNewContext(expression, { auditResult: result }, { timeout: 1000 }), result !== "success");
  });
}

test("capability checks do not expand the workflow token permissions", () => {
  const permissions = workflow.match(/^permissions:\n((?: {2}[^\n]*\n)+)/mu)?.[1];
  assert.equal(permissions, "  contents: read\n  security-events: write\n");
  assert.doesNotMatch(reviewJob, /^    permissions:/mu);
  assert.match(capability, /^        id: capability$/mu);
  assert.match(capability, /^        uses: actions\/github-script@[a-f0-9]{40}/mu);
  assert.doesNotMatch(capability, /^          github-token:/mu);
});
