import { useEffect, useRef, useState, type FormEvent, type KeyboardEvent } from "react";
import type { InstanceType, Region, UiState } from "../types";
import {
  cleanName,
  computeHourly,
  diskMonthly,
  hourly,
  INSTANCE_TYPES,
  monthly,
  NAME_RE,
  REGIONS,
  shapeFits,
  sizeOf,
  suggestName,
} from "../machine";
import { Icon } from "./Icons";
import { ProjectList, useLocalProjects } from "./ProjectPicker";
import { SubHeader } from "./SubHeader";

export interface NewMachineSpec {
  name: string;
  size: string;
  instanceType: InstanceType;
  region: Region;
  diskGb: number;
  projects: string[];
}

interface Props {
  state: UiState;
  onNameChange: (name: string) => void;
  onCreate: (spec: NewMachineSpec) => void;
  onBack: () => void;
}

const clamp = (n: number, lo: number, hi: number) => Math.min(hi, Math.max(lo, n));

export function NewMachine({ state, onNameChange, onCreate, onBack }: Props) {
  const taken = state.machines.map((m) => m.name);
  const [name, setName] = useState(() => suggestName(taken));
  const [instanceType, setInstanceType] = useState<InstanceType>("default");
  // the entry shape: room for builds and tests without the bigger price
  const [size, setSize] = useState(() => (state.sizes.some((s) => s.id === "medium") ? "medium" : (state.sizes[0]?.id ?? "")));
  const [region, setRegion] = useState<Region>("us");
  const shape = sizeOf(state.sizes, size);
  const [diskGb, setDiskGb] = useState(() => shape?.defaultDisk ?? 0);
  const input = useRef<HTMLInputElement>(null);
  const projects = useLocalProjects();
  const [picked, setPicked] = useState<string[]>([]);
  const toggle = (id: string) => setPicked((p) => (p.includes(id) ? p.filter((x) => x !== id) : [...p, id]));

  useEffect(() => {
    input.current?.select();
  }, []);
  useEffect(() => onNameChange(name), [name, onNameChange]);

  const pickSize = (id: string) => {
    const s = sizeOf(state.sizes, id);
    setSize(id);
    if (s) setDiskGb(s.defaultDisk);
  };
  const pickType = (t: InstanceType) => {
    setInstanceType(t);
    // `max` needs a VM: fall back to the biggest shape a container can be
    if (shape && !shapeFits(shape, t)) {
      const fallback = [...state.sizes].reverse().find((s) => shapeFits(s, t));
      if (fallback) pickSize(fallback.id);
    }
  };

  const disk = shape ? clamp(diskGb, shape.minDisk, shape.maxDisk) : diskGb;

  const problem = !name
    ? "Give it a name"
    : !NAME_RE.test(name)
      ? "Start with a letter or number"
      : taken.includes(name)
        ? "You already have a machine with this name"
        : null;

  const submit = (e: FormEvent) => {
    e.preventDefault();
    if (!problem && shape) onCreate({ name, size, instanceType, region, diskGb: disk, projects: picked });
  };

  return (
    <form className="view" onSubmit={submit}>
      <SubHeader title="New machine" onBack={onBack} />
      <div className="view-body">
        <div className="field">
          <label className="caption" htmlFor="machine-name">
            name
          </label>
          <span className="prompt-input">
            <span className="prompt-sign" aria-hidden>
              $
            </span>
            <input
              id="machine-name"
              ref={input}
              aria-describedby="machine-name-hint"
              value={name}
              onChange={(e) => setName(cleanName(e.target.value))}
              spellCheck={false}
              autoComplete="off"
              aria-invalid={!!problem}
            />
          </span>
          <span id="machine-name-hint" className={`hint${problem && name ? " is-error" : ""}`}>
            {problem ?? "Shows up under this name in T3 Code"}
          </span>
        </div>

        <fieldset className="field">
          <legend className="caption">type</legend>
          <div className="sizes" role="radiogroup">
            {INSTANCE_TYPES.map((t) => (
              <label key={t.id} className={`size${instanceType === t.id ? " is-on" : ""}`}>
                <input
                  type="radio"
                  name="instance-type"
                  value={t.id}
                  checked={instanceType === t.id}
                  onChange={() => pickType(t.id)}
                />
                <span className="size-name">{t.label}</span>
                <span className="size-spec">{t.spec}</span>
                <span className="size-price">{t.multiplier}× compute</span>
              </label>
            ))}
          </div>
        </fieldset>

        <fieldset className="field">
          <legend className="caption">size</legend>
          <div className="sizes" role="radiogroup">
            {state.sizes.map((s) => {
              const fits = shapeFits(s, instanceType);
              return (
                <label
                  key={s.id}
                  className={`size${size === s.id ? " is-on" : ""}${fits ? "" : " is-off"}`}
                  title={fits ? undefined : "Needs a vm or performance machine"}
                >
                  <input
                    type="radio"
                    name="size"
                    value={s.id}
                    checked={size === s.id}
                    disabled={!fits}
                    onChange={() => pickSize(s.id)}
                  />
                  <span className="size-name">{s.id}</span>
                  <span className="size-spec">{s.spec}</span>
                  <span className="size-price">{fits ? hourly(computeHourly(s, instanceType)) : "vm only"}</span>
                </label>
              );
            })}
          </div>
          <span className="hint">Compute bills only while awake · sleeps 3 min after agents finish · resize anytime</span>
        </fieldset>

        <fieldset className="field">
          <legend className="caption">region</legend>
          <div className="segmented" role="radiogroup" aria-label="Region">
            {REGIONS.map((r) => (
              <button
                key={r.id}
                type="button"
                role="radio"
                aria-checked={region === r.id}
                className={region === r.id ? "is-on" : undefined}
                onClick={() => setRegion(r.id)}
              >
                {r.label}
              </button>
            ))}
          </div>
        </fieldset>

        {shape ? (
          <fieldset className="field">
            <legend className="caption">disk</legend>
            <div className="disk-pick">
              <input
                type="range"
                className="range"
                aria-label="Disk size in GB"
                min={shape.minDisk}
                max={shape.maxDisk}
                step={1}
                value={disk}
                disabled={shape.minDisk === shape.maxDisk}
                onChange={(e) => setDiskGb(Number(e.target.value))}
              />
              <span className="disk-pick-value">{disk} GB</span>
            </div>
            <span className="hint">
              {disk} GB · {monthly(diskMonthly(disk))} even while asleep · disk can grow, never shrink
            </span>
          </fieldset>
        ) : null}

        <fieldset className="field">
          <legend className="caption">projects</legend>
          <div className="pick pick-checklist" role="group" aria-label="Projects to add">
            <div className="pick-scroll">
              <ProjectList
                projects={projects}
                shown={projects.list ?? []}
                query=""
                option={(p) => {
                  const on = picked.includes(p.id);
                  // One clone per repository: another project of the same repo is already picked.
                  const sameRepo = !on && (projects.list ?? []).some((o) => o.repoKey === p.repoKey && picked.includes(o.id));
                  const flip = () => !sameRepo && toggle(p.id);
                  return {
                    role: "checkbox",
                    tabIndex: 0,
                    "aria-checked": on,
                    "aria-disabled": sameRepo,
                    className: `pick-option${on ? " is-active" : ""}${sameRepo ? " is-done" : ""}`,
                    onClick: flip,
                    onKeyDown: (e: KeyboardEvent) => {
                      if (e.key === " " || e.key === "Enter") {
                        e.preventDefault();
                        flip();
                      }
                    },
                    badge: on ? (
                      <span className="line-ok">
                        <Icon name="check" size={10} />
                      </span>
                    ) : null,
                  };
                }}
              />
            </div>
          </div>
          <span className="hint">Optional · cloned from git once it's up, .env files included</span>
        </fieldset>
      </div>
      <div className="view-foot">
        <button type="submit" className="btn btn-solid btn-lg" disabled={!!problem || !shape}>
          Launch machine
        </button>
        <p className="foot-note">
          Takes about a minute{picked.length ? `, then ${picked.length} ${picked.length === 1 ? "project" : "projects"}` : ""}. T3 Code
          will ask to add it.
        </p>
      </div>
    </form>
  );
}
