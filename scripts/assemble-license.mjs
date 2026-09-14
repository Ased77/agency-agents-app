#!/usr/bin/env node
/**
 * assemble-license.mjs — turn a signed payload into the blob a customer pastes
 * into Settings → AI provider.
 *
 * This script holds **no key material** and never signs anything. Signing is
 * done with the real `minisign` CLI against a private key that lives offline
 * (see docs/provider-license.md); this only joins the two halves into the
 * single text blob the app expects:
 *
 *   <base64(payload bytes)>
 *   untrusted comment: …
 *   <base64 signature>
 *   trusted comment: …
 *   <base64 global signature>
 *
 * Usage:
 *   node scripts/assemble-license.mjs payload.json payload.json.minisig > license.txt
 */

import { readFileSync } from "node:fs";

const [, , payloadPath, signaturePath] = process.argv;

if (!payloadPath || !signaturePath) {
  console.error(
    "usage: node scripts/assemble-license.mjs <payload.json> <payload.json.minisig>",
  );
  process.exit(2);
}

const payload = readFileSync(payloadPath);
const signature = readFileSync(signaturePath, "utf8");

/** The signature block is exactly the four non-empty lines minisign writes. */
const signatureLines = signature
  .split(/\r?\n/)
  .map((line) => line.trim())
  .filter(Boolean);

if (signatureLines.length !== 4) {
  console.error(
    `expected a 4-line minisign signature block, got ${signatureLines.length} lines`,
  );
  process.exit(1);
}
if (!signatureLines[0].startsWith("untrusted comment:")) {
  console.error("signature block does not start with an `untrusted comment:` line");
  process.exit(1);
}

// Validate that the payload parses before shipping it — a typo here becomes a
// support ticket, since a malformed payload fails signature verification
// offline with no server-side diagnostic.
let parsed;
try {
  parsed = JSON.parse(payload.toString("utf8"));
} catch (error) {
  console.error(`payload is not valid JSON: ${error.message}`);
  process.exit(1);
}
for (const field of ["v", "licenseId", "plan", "meter", "expiresAt"]) {
  if (parsed[field] === undefined) {
    console.error(`payload is missing required field: ${field}`);
    process.exit(1);
  }
}
if (!["tokens", "time", "both"].includes(parsed.meter)) {
  console.error(`meter must be tokens | time | both, got ${parsed.meter}`);
  process.exit(1);
}

process.stdout.write(
  `${payload.toString("base64")}\n${signatureLines.join("\n")}\n`,
);
console.error(
  `assembled ${parsed.plan} license (${parsed.meter}) for ${parsed.licenseId}, ` +
    `expires ${parsed.expiresAt === 0 ? "never" : new Date(parsed.expiresAt * 1000).toISOString()}`,
);
