import { useSearchParams } from "react-router";
import { HomeSurface } from "../app/shell";

export default function Home() {
  const [params] = useSearchParams();
  return <HomeSurface preview={params.get("preview") === "1"} />;
}
