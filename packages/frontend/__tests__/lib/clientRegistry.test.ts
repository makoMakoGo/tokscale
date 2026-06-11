import { execFileSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { CLIENT_REGISTRY, BASE_CLIENT_TYPES } from "../../src/lib/clientRegistry.generated";
import { validateSubmission } from "../../src/lib/validation/submission";

type CatalogEntry = {
  id: string;
  displayName: string;
  shortName: string;
  logo: string;
  color: string;
  textColor?: string;
  submitDefault: boolean;
};

const repoRoot = fileURLToPath(new URL("../../../..", import.meta.url));
const catalogPath = fileURLToPath(
  new URL("../../../../crates/tokscale-core/client-catalog.json", import.meta.url)
);

function catalogEntries(): CatalogEntry[] {
  return JSON.parse(readFileSync(catalogPath, "utf8")) as CatalogEntry[];
}

function payloadForClient(client: string) {
  return {
    meta: {
      generatedAt: "2024-12-02T00:00:00.000Z",
      version: "2.1.1",
      dateRange: { start: "2024-12-01", end: "2024-12-01" },
    },
    summary: {
      totalTokens: 1500,
      totalCost: 1.5,
      totalDays: 1,
      activeDays: 1,
      averagePerDay: 1.5,
      maxCostInSingleDay: 1.5,
      clients: [client],
      models: ["claude-sonnet-4"],
    },
    years: [
      {
        year: "2024",
        totalTokens: 1500,
        totalCost: 1.5,
        range: { start: "2024-12-01", end: "2024-12-01" },
      },
    ],
    contributions: [
      {
        date: "2024-12-01",
        totals: { tokens: 1500, cost: 1.5, messages: 5 },
        intensity: 2,
        tokenBreakdown: {
          input: 1000,
          output: 500,
          cacheRead: 0,
          cacheWrite: 0,
          reasoning: 0,
        },
        clients: [
          {
            client,
            modelId: "claude-sonnet-4",
            tokens: {
              input: 1000,
              output: 500,
              cacheRead: 0,
              cacheWrite: 0,
              reasoning: 0,
            },
            cost: 1.5,
            messages: 5,
          },
        ],
      },
    ],
  };
}

describe("frontend client registry", () => {
  it("generated registry is up to date", () => {
    expect(() => {
      execFileSync("bun", ["scripts/generate-client-registry.ts", "--check"], {
        cwd: repoRoot,
        stdio: "pipe",
      });
    }).not.toThrow();
  });

  it("matches the core client catalog", () => {
    const catalog = catalogEntries();
    expect(BASE_CLIENT_TYPES).toEqual(catalog.map((entry) => entry.id));

    for (const entry of catalog) {
      expect(CLIENT_REGISTRY[entry.id as keyof typeof CLIENT_REGISTRY]).toEqual({
        displayName: entry.displayName,
        shortName: entry.shortName,
        logo: entry.logo,
        color: entry.color,
        textColor: entry.textColor,
        submitDefault: entry.submitDefault,
      });
    }
  });

  it("accepts trae submissions", () => {
    const result = validateSubmission(payloadForClient("trae"));

    expect(result.valid).toBe(true);
    expect(result.errors).toEqual([]);
  });

  it("accepts every base client id in submission validation", () => {
    const rejected = BASE_CLIENT_TYPES.filter((client) => {
      const result = validateSubmission(payloadForClient(client));
      return !result.valid;
    });

    expect(rejected).toEqual([]);
  });

  it("preserves every base client id in submission validation", () => {
    for (const client of BASE_CLIENT_TYPES) {
      const result = validateSubmission(payloadForClient(client));

      expect(result.valid).toBe(true);
      expect(result.data?.summary.clients).toEqual([client]);
      expect(result.data?.contributions[0]?.clients[0]?.client).toBe(client);
    }
  });
});
