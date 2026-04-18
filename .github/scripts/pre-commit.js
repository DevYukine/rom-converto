const { execSync } = require("child_process");

exports.preCommit = () => {
  execSync("cargo update --workspace", { stdio: "inherit" });
};
