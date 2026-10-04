#!/usr/bin/env node
// Records the demo video's frames from the running simulator.
//
// For each scene in scenes.json this drives the simulator page with Playwright the way a
// person would (type the question, send it, wait for the answer, tap a horse on the MCP App)
// and saves a screenshot at each step: idle, typing, thinking, answer, tap. render.py turns
// those frames into video, holding each for as long as the voices need. Keyframes rather than
// a screen recording keep the cut deterministic: Bedrock's latency never shows, and a re-cut
// after a race day is one command.
//
// Environment: TRACKSIDE_SIM_URL (default from scenes.json), VIDEO_WORK (output dir, default
// ./work), SCENES (comma-separated scene ids to record, default all).

const { chromium } = require("playwright");
const fs = require("node:fs");
const path = require("node:path");
const { execFileSync } = require("node:child_process");

const here = __dirname;
const cfg = JSON.parse(fs.readFileSync(path.join(here, "scenes.json"), "utf8"));
const work = process.env.VIDEO_WORK || path.join(here, "work");
const framesDir = path.join(work, "frames");
fs.mkdirSync(framesDir, { recursive: true });
const simUrl = process.env.TRACKSIDE_SIM_URL || cfg.sim_url;
const only = process.env.SCENES ? new Set(process.env.SCENES.split(",")) : null;
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const log = (...a) => console.error(new Date().toISOString().slice(11, 19), ...a);

// The listener's memory (see awsx.py): "forget" empties it, "last-checked:YYYY-MM-DD" backdates
// the day they last heard their stable report, so the next report opens with a catch-up.
function memory(op) {
  const args = op === "forget" ? ["memory", "forget"] : ["memory", "last-checked", op.split(":")[1]];
  log("memory", ...args);
  execFileSync("python3", [path.join(here, "awsx.py"), ...args], { stdio: "inherit" });
}

async function main() {
  // fonts.conf makes Chromium draw the simulator's system-ui in Inter, like the cards.
  const browser = await chromium.launch({ env: { ...process.env, FONTCONFIG_FILE: path.join(here, "fonts.conf") } });
  const sim = await browser.newContext({
    viewport: cfg.viewport,
    deviceScaleFactor: cfg.scale,
    colorScheme: "dark",
    locale: "en-AU",
    timezoneId: "Australia/Melbourne",
  });
  const page = await sim.newPage();
  page.on("pageerror", (e) => log("page error:", e.message));
  const cards = await browser.newContext({ viewport: { width: 1920, height: 1080 }, deviceScaleFactor: 1 });

  // With SCENES set, only those scenes are re-recorded and the rest of the timeline is kept.
  const timelineFile = path.join(work, "timeline.json");
  const previous = only && fs.existsSync(timelineFile) ? JSON.parse(fs.readFileSync(timelineFile, "utf8")) : [];
  const timeline = [];
  let loaded = false;
  for (const scene of cfg.scenes) {
    if (only && !only.has(scene.id)) {
      const kept = previous.find((s) => s.id === scene.id);
      if (kept) timeline.push(kept);
      continue;
    }
    log("scene", scene.id);
    if (scene.type === "card") {
      const p = await cards.newPage();
      await p.goto("file://" + path.join(here, "cards", scene.card + ".html"));
      await p.evaluate(() => document.fonts.ready);
      await sleep(300);
      const file = path.join(framesDir, `${scene.id}.png`);
      await p.screenshot({ path: file });
      await p.close();
      timeline.push({ ...scene, frames: [{ kind: "card", file }] });
      continue;
    }
    let result = null;
    for (let attempt = 1; attempt <= 5 && !result; attempt++) {
      if (scene.memory) memory(scene.memory);
      if (scene.reload || !loaded || attempt > 1) {
        await open(page);
        loaded = true;
      }
      try {
        result = await ask(page, scene);
        // The model is free to phrase the answer, but it has to be about the right thing.
        if (scene.expect && !new RegExp(scene.expect, "i").test(result.answer)) {
          log(`answer doesn't match /${scene.expect}/, asking again`);
          result = null;
          await sleep(3000);
        }
      } catch (e) {
        log(`attempt ${attempt} failed: ${e.message}`);
        if (attempt === 5) throw e;
        await sleep(20000 * attempt); // Bedrock throttling clears in well under a minute
      }
    }
    if (!result) throw new Error(`${scene.id}: no acceptable answer after 5 attempts`);
    timeline.push(result);
  }
  fs.writeFileSync(timelineFile, JSON.stringify(timeline, null, 2));
  log("wrote", timelineFile);
  await browser.close();
}

async function open(page) {
  await page.goto(simUrl);
  await page.waitForSelector("#q");
  await page.waitForFunction(() => {
    const row = document.querySelector("#link-row");
    return row && !row.textContent.includes("Checking");
  });
  await sleep(400);
}

// Waits for the screen to finish drawing: the MCP App's iframe reports its height once it has
// rendered, and the page's own cards are synchronous.
async function settled(page) {
  await sleep(800);
  await page
    .waitForFunction(
      () => {
        const f = document.querySelector(".app-frame iframe");
        return !f || f.style.height;
      },
      null,
      { timeout: 8000 },
    )
    .catch(() => log("app frame never reported a size"));
  await sleep(700);
}

async function ask(page, scene) {
  const frames = [];
  const shot = async (kind) => {
    const file = path.join(framesDir, `${scene.id}-${String(frames.length).padStart(2, "0")}-${kind}.png`);
    await page.screenshot({ path: file });
    frames.push({ kind, file });
  };
  await page.mouse.move(0, 0); // park the pointer on the banner so no row shows a hover state
  await page.fill("#q", "");
  await shot("idle");
  const q = scene.ask;
  const steps = 10;
  for (let i = 1; i <= steps; i++) {
    await page.fill("#q", q.slice(0, Math.ceil((q.length * i) / steps)));
    await shot("typing");
  }
  const answers = () => page.locator("#transcript .msg.assistant").count();
  const errors = () => page.locator("#transcript .msg.error").count();
  const [a0, e0] = [await answers(), await errors()];
  await page.press("#q", "Enter");
  await page.waitForSelector('#device[data-state="thinking"]', { timeout: 5000 }).catch(() => {});
  await sleep(150);
  await shot("thinking");
  await page.waitForFunction(
    ([a, e]) =>
      document.querySelectorAll("#transcript .msg.assistant").length > a ||
      document.querySelectorAll("#transcript .msg.error").length > e,
    [a0, e0],
    { timeout: 150000 },
  );
  if ((await errors()) > e0) {
    throw new Error("simulator error: " + (await page.locator("#transcript .msg.error").last().textContent()));
  }
  await settled(page);
  await shot("answer");
  const answer = (await page.locator("#transcript .msg.assistant").last().textContent()).trim();
  const tools = await page.locator("#trace > div").allTextContents();
  log("answer:", answer);
  for (const act of scene.then || []) {
    if (act.tap) {
      const app = page.frameLocator('iframe[title="Trackside MCP App"]');
      const row = app.locator(`[data-horse="${act.tap}"]`).first();
      await row.scrollIntoViewIfNeeded();
      await row.hover(); // the row's real :hover state, so the tap reads as an interaction
      await sleep(250);
      await shot("hover");
      const before = await page.locator("#trace > div").count();
      await row.click();
      await page.mouse.move(0, 0);
      await page.waitForFunction((n) => document.querySelectorAll("#trace > div").length > n, before, { timeout: 60000 });
      await settled(page);
      // The App appends the form below the race card; bring it to the top of the screen.
      const drawerTop = await app.locator("#drawer").evaluate((d) => d.getBoundingClientRect().top);
      await page.evaluate((top) => {
        const screen = document.getElementById("screen");
        const frame = document.querySelector(".app-frame iframe");
        screen.scrollTop += frame.getBoundingClientRect().top - screen.getBoundingClientRect().top + top + 2;
      }, drawerTop);
      await sleep(500);
      await shot("tap");
      log("tapped", act.tap, "→", (await page.locator("#trace > div").last().textContent()).slice(0, 80));
    }
  }
  return { ...scene, answer, tools, frames };
}

main().catch((e) => {
  console.error(e);
  process.exit(1);
});
