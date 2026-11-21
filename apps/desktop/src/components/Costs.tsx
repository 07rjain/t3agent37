import type { Costs as CostsData } from "../types";
import { money } from "../machine";

/** Below this the wallet may run dry before the month's disk is paid. */
const LOW_WALLET = 5;

export function Costs({ costs }: { costs: CostsData }) {
  const ram = costs.memoryLimitGb > 0 ? costs.awakeRamGb / costs.memoryLimitGb : 0;
  return (
    <footer className="costs" aria-label="Costs">
      <div className={`cost${costs.walletBalance < LOW_WALLET ? " is-warn" : ""}`} title="Agent37 wallet balance">
        <span className="caption">wallet</span>
        <span className="cost-value">{money(costs.walletBalance)}</span>
      </div>
      <div className="cost" title="Every machine's disk per month: billed even while all of them sleep">
        <span className="caption">disk/mo</span>
        <span className="cost-value">{money(costs.idleMonthly)}</span>
      </div>
      <div className="cost">
        <span className="caption">this month</span>
        <span className="cost-value">{money(costs.month)}</span>
      </div>
      <div className={`cost${ram >= 0.8 ? " is-warn" : ""}`} title="Memory of the machines awake now / your Agent37 limit">
        <span className="caption">awake ram</span>
        <span className="cost-value">
          {costs.awakeRamGb}
          <span className="cost-of">/{costs.memoryLimitGb} GB</span>
        </span>
      </div>
    </footer>
  );
}
