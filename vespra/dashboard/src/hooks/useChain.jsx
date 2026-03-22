import { createContext } from "preact";
import { useState, useContext } from "preact/hooks";

const CHAINS = [
  { id: "all", label: "All Chains" },
  { id: "ethereum", label: "Ethereum" },
  { id: "base", label: "Base" },
  { id: "arbitrum", label: "Arbitrum" },
  { id: "optimism", label: "Optimism" },
  { id: "sepolia", label: "Sepolia" },
  { id: "base_sepolia", label: "Base Sepolia" },
  { id: "arbitrum_sepolia", label: "Arb Sepolia" },
];

const ChainContext = createContext();

export function ChainProvider({ children }) {
  const [chain, setChain] = useState("all");
  return (
    <ChainContext.Provider value={{ chain, setChain, chains: CHAINS }}>
      {children}
    </ChainContext.Provider>
  );
}

export function useChain() {
  return useContext(ChainContext);
}
