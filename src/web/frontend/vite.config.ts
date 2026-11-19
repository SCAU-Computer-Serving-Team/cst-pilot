import { createReadStream, existsSync, statSync } from "node:fs";
import { extname, isAbsolute, relative, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";
import { reactRouter } from "@react-router/dev/vite";
import tailwindcss from "@tailwindcss/vite";
import { defineConfig, type Plugin } from "vite";

const sharedFontsDir = resolve(fileURLToPath(new URL("../../../assets/fonts", import.meta.url)));

function sharedFontsDevPlugin(): Plugin {
  return {
    name: "cst-shared-fonts-dev",
    configureServer(server) {
      server.middlewares.use("/fonts", (req, res, next) => {
        if (req.method !== "GET" && req.method !== "HEAD") {
          next();
          return;
        }
        const rawPath = decodeURIComponent((req.url ?? "/").split("?")[0]);
        const target = resolve(sharedFontsDir, `.${rawPath}`);
        const rel = relative(sharedFontsDir, target);
        if (!rel || rel === ".." || rel.startsWith(`..${sep}`) || isAbsolute(rel) || !existsSync(target)) {
          next();
          return;
        }
        const info = statSync(target);
        if (!info.isFile()) {
          next();
          return;
        }
        const ext = extname(target).toLowerCase();
        res.setHeader(
          "Content-Type",
          ext === ".otf" ? "font/otf" : ext === ".txt" ? "text/plain; charset=utf-8" : "application/octet-stream",
        );
        res.setHeader("Content-Length", info.size);
        if (req.method === "HEAD") {
          res.end();
          return;
        }
        createReadStream(target).pipe(res);
      });
    },
  };
}

export default defineConfig({
  plugins: [sharedFontsDevPlugin(), tailwindcss(), reactRouter()],
});
