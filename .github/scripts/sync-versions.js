const fs = require("fs");
const { execSync } = require("child_process");

const cliToml = "crates/rom-converto-cli/Cargo.toml";
const content = fs.readFileSync(cliToml, "utf8");
const match = content.match(/^version = "(.+)"/m);
if (!match) {
  console.error("Could not read version from", cliToml);
  process.exit(1);
}

const version = match[1];
console.log("Syncing version", version, "to all crates...");

function updateToml(path) {
  const data = fs.readFileSync(path, "utf8");
  fs.writeFileSync(path, data.replace(/^version = ".*"/m, `version = "${version}"`));
}

function updateJson(path) {
  const data = fs.readFileSync(path, "utf8");
  fs.writeFileSync(path, data.replace(/"version": ".*"/, `"version": "${version}"`));
}

updateToml("crates/rom-converto-lib/Cargo.toml");
updateToml("crates/rom-converto-gui/src-tauri/Cargo.toml");
updateJson("crates/rom-converto-gui/src-tauri/tauri.conf.json");

execSync("git add crates/rom-converto-lib/Cargo.toml crates/rom-converto-gui/src-tauri/Cargo.toml crates/rom-converto-gui/src-tauri/tauri.conf.json");

console.log("All versions synced to", version);
