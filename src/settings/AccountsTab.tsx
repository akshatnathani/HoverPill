import { useEffect, useState, type ReactNode } from "react";
import { chatGptModels, connectChatGpt, connectGoogle, disconnectChatGpt, disconnectGoogle, openChatGptUsage } from "../lib/bridge";
import { useAccounts } from "../lib/useAccounts";
import { RANGES, type AccountStatus, type ModelInfo, type Settings } from "../lib/types";
import { Row, Section, SelectRow, SliderRow, SwitchRow } from "./controls";

type Update = (change: (s: Settings) => Settings, debounceMs?: number) => void;

const AUTO_MODEL = "__auto__";

/** Keys for the ChatGPT shortcut. Each must parse in Rust (see the test in shortcut.rs). */
const NO_SHORTCUT = "__none__";
const SHORTCUTS = [
  { value: "Ctrl+Alt+G", label: "Ctrl + Alt + G" },
  { value: "Ctrl+Shift+Space", label: "Ctrl + Shift + Space" },
  { value: "Alt+Shift+Space", label: "Alt + Shift + Space" },
  { value: "Ctrl+Alt+Space", label: "Ctrl + Alt + Space" },
  { value: "Alt+Space", label: "Alt + Space" },
  { value: NO_SHORTCUT, label: "Off" },
];

/** Google Calendar and ChatGPT: connect, disconnect, and the choices that go with each. */
export function AccountsTab({ settings, update }: { settings: Settings; update: Update }) {
  const { google, chatgpt } = useAccounts();
  const [problem, setProblem] = useState<string | null>(null);
  // Sign-ins report their own errors through the account status; this catches anything else.
  const run = (action: () => Promise<void>) => {
    setProblem(null);
    action().catch((e) => setProblem(String(e)));
  };

  return (
    <>
      {problem && (
        <div className="banner" role="alert">
          {problem}
        </div>
      )}
      <GoogleSection account={google} settings={settings} update={update} run={run} />
      <ChatGptSection account={chatgpt} settings={settings} update={update} run={run} />
    </>
  );
}

function ConnectButton(props: { account: AccountStatus; connect: string; onConnect: () => void; onDisconnect: () => void }) {
  if (props.account.busy)
    return (
      <button type="button" className="button" disabled>
        Waiting for the browser…
      </button>
    );
  return props.account.connected ? (
    <button type="button" className="button" onClick={props.onDisconnect}>
      Disconnect
    </button>
  ) : (
    <button type="button" className="button is-primary" onClick={props.onConnect}>
      {props.connect}
    </button>
  );
}

function Status({ account, connected, idle }: { account: AccountStatus; connected: ReactNode; idle: ReactNode }) {
  if (account.error) return <span className="row-error">{account.error}</span>;
  if (account.busy) return <>Finish signing in in your browser, then come back here.</>;
  return <>{account.connected ? connected : idle}</>;
}

// ---------- Google Calendar ----------

function GoogleSection({ account, settings, update, run }: { account: AccountStatus; settings: Settings; update: Update; run: (a: () => Promise<void>) => void }) {
  const c = settings.calendar;
  const setCalendar = (patch: Partial<Settings["calendar"]>, debounceMs = 0) =>
    update((s) => ({ ...s, calendar: { ...s.calendar, ...patch } }), debounceMs);
  return (
    <>
      <Section title="Google Calendar">
        <Row
          title="Google Calendar"
          description={
            <Status
              account={account}
              connected={<>Connected{account.email ? ` as ${account.email}` : ""}. Events come from the calendars you show in Google Calendar.</>}
              idle={<>See your next event on the notch's Home, with a reminder before it starts. HoverPill only reads your calendars.</>}
            />
          }
        >
          <ConnectButton account={account} connect="Connect" onConnect={() => run(connectGoogle)} onDisconnect={() => run(disconnectGoogle)} />
        </Row>
        <SwitchRow
          title="Show events in the notch"
          description="The next event on Home, and a dot on each day that has something on."
          checked={c.enabled}
          onChange={(enabled) => setCalendar({ enabled })}
        />
        <SliderRow
          title="Remind me before events"
          description="A card drops down from the notch, like a notification."
          value={c.reminderMinutes}
          {...RANGES.reminderMinutes}
          format={(v) => (v === 0 ? "Off" : `${v} min`)}
          onChange={(reminderMinutes) => setCalendar({ reminderMinutes }, 150)}
        />
      </Section>

    </>
  );
}

// ---------- ChatGPT ----------

function ChatGptSection({ account, settings, update, run }: { account: AccountStatus; settings: Settings; update: Update; run: (a: () => Promise<void>) => void }) {
  const [models, setModels] = useState<ModelInfo[] | null>(null);
  const ready = account.connected && account.planUsage;
  useEffect(() => {
    if (!ready) return setModels(null);
    chatGptModels().then(setModels, () => setModels([]));
  }, [ready]);

  const model = settings.chatgpt.model;
  const options = [
    { value: AUTO_MODEL, label: "Default for your plan" },
    ...(models ?? []).map((m) => ({ value: m.id, label: m.label })),
    ...(model && models && !models.some((m) => m.id === model) ? [{ value: model, label: `${model} (not offered now)` }] : []),
  ];

  return (
    <Section title="ChatGPT">
      <Row
        title="ChatGPT"
        description={
          <Status
            account={account}
            connected={
              account.planUsage ? (
                <>Signed in{account.email ? ` as ${account.email}` : ""}. Questions in the notch's ChatGPT tab use your plan's included usage.</>
              ) : (
                <>
                  Signed in, but HoverPill isn't allowed to use your ChatGPT plan, so it can't answer.{" "}
                  <button type="button" className="link" onClick={() => run(() => connectChatGpt(true))}>
                    Allow plan usage
                  </button>
                </>
              )
            }
            idle={<>Sign in with your ChatGPT account to ask questions from the notch. Uses your Plus or Pro plan; no API key needed.</>}
          />
        }
      >
        <ConnectButton account={account} connect="Sign in with ChatGPT" onConnect={() => run(() => connectChatGpt())} onDisconnect={() => run(disconnectChatGpt)} />
      </Row>
      {ready && (
        <>
          <SelectRow
            title="Model"
            description="The models your ChatGPT plan offers to apps."
            value={model ?? AUTO_MODEL}
            options={options}
            onChange={(v) => update((s) => ({ ...s, chatgpt: { ...s.chatgpt, model: v === AUTO_MODEL ? null : v } }))}
          />
          <SelectRow
            title="Shortcut"
            description="Opens the notch's ChatGPT tab from any app, ready to type. Esc or a click elsewhere closes it."
            value={settings.chatgpt.shortcut || NO_SHORTCUT}
            options={SHORTCUTS}
            onChange={(v) => update((s) => ({ ...s, chatgpt: { ...s.chatgpt, shortcut: v === NO_SHORTCUT ? "" : v } }))}
          />
          <Row title="Usage" description="Answers count against your ChatGPT plan's limits, the same as using ChatGPT itself.">
            <button type="button" className="button" onClick={openChatGptUsage}>
              See usage
            </button>
          </Row>
        </>
      )}
      <div className="row is-text">
        <div className="row-description">
          Conversations aren't saved by HoverPill, and are sent with storage turned off. You can remove HoverPill's access any time in
          ChatGPT's settings.
        </div>
      </div>
    </Section>
  );
}
