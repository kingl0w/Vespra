import { useState, useEffect, useCallback, useRef } from "preact/hooks";

export function useApi(fetcher, deps = []) {
  const [data, setData] = useState(null);
  const [error, setError] = useState(null);
  const [loading, setLoading] = useState(true);

  const refresh = useCallback(() => {
    setLoading(true);
    setError(null);
    fetcher()
      .then(setData)
      .catch(setError)
      .finally(() => setLoading(false));
  }, deps);

  useEffect(() => {
    refresh();
  }, [refresh]);

  return { data, error, loading, refresh };
}

export function usePolling(fetcher, intervalMs, deps = []) {
  const result = useApi(fetcher, deps);
  const refreshRef = useRef(result.refresh);
  refreshRef.current = result.refresh;

  useEffect(() => {
    let id = setInterval(() => refreshRef.current(), intervalMs);

    const onVisibility = () => {
      if (document.hidden) {
        clearInterval(id);
        id = null;
      } else {
        refreshRef.current(); // fetch fresh data on return
        id = setInterval(() => refreshRef.current(), intervalMs);
      }
    };

    document.addEventListener("visibilitychange", onVisibility);
    return () => {
      clearInterval(id);
      document.removeEventListener("visibilitychange", onVisibility);
    };
  }, [intervalMs]);

  return result;
}
