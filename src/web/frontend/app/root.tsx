import { Links, Meta, Outlet, Scripts, ScrollRestoration } from "react-router";
import stylesheet from "./styles.css?url";

export function links() {
  return [{ rel: "stylesheet", href: stylesheet }];
}

export function Layout({ children }: { children: React.ReactNode }) {
  return (
    <html lang="zh-CN" suppressHydrationWarning>
      <head>
        <meta charSet="utf-8" />
        <meta name="viewport" content="width=device-width, initial-scale=1" />
        <Meta />
        <Links />
      </head>
      <body>
        {children}
        <ScrollRestoration />
        <Scripts />
      </body>
    </html>
  );
}

export function HydrateFallback() {
  return <main>正在载入 CST Pilot…</main>;
}

export default function App() {
  return <Outlet />;
}
