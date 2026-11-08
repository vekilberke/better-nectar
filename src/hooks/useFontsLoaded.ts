import { useEffect, useState } from "react";

/** Changes when web fonts finish loading, so canvas measurements taken with the fallback font run again. */
export function useFontsLoaded(): number {
  const [loaded, setLoaded] = useState(0);
  useEffect(() => {
    const fonts = typeof document !== "undefined" ? document.fonts : undefined;
    if (!fonts) return;
    let alive = true;
    const bump = () => {
      if (alive) setLoaded((n) => n + 1);
    };
    fonts.ready.then(bump, () => {});
    fonts.addEventListener("loadingdone", bump);
    return () => {
      alive = false;
      fonts.removeEventListener("loadingdone", bump);
    };
  }, []);
  return loaded;
}
