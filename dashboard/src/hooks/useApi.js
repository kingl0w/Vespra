import { useState, useEffect, useCallback } from "preact/hooks";

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

  useEffect(() => {
    const id = setInterval(result.refresh, intervalMs);
    return () => clearInterval(id);
  }, [result.refresh, intervalMs]);

  return result;
}
