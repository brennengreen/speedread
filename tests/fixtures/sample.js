const express = require("express");

/**
 * Build the app.
 */
function createApp(config) {
  const app = express();
  app.use(express.json());
  app.get("/health", (req, res) => res.send("ok"));
  return app;
}

class Cache {
  constructor(size) {
    this.size = size;
    this.map = new Map();
    this.hits = 0;
  }

  get(key) {
    const v = this.map.get(key);
    if (v !== undefined) this.hits++;
    return v;
  }
}

module.exports = {
  createApp,
  start: async (port) => {
    const app = createApp({});
    await app.listen(port);
    return app;
  },
};
