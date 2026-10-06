import { useSearchParams } from "react-router";
import { HomeSurface } from "../shell/shell";

export default function Home() {
	const [params] = useSearchParams();
	return <HomeSurface preview={params.get("preview") === "1"} externalBackground />;
}
