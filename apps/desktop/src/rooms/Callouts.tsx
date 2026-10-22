// A room's state worth attention, at the top of the room's page: no
// password, catching up after a restore, handing the room over, joining
// paused (M13, P4).
import { useEffect, useState } from "react";
import type { RoomView } from "../bindings/RoomView";
import { namesListed, owner } from "../model/rooms";
import { Button } from "../ui/Button";
import { Callout } from "../ui/Callout";
import { duration } from "../ui/text";
import type { RoomActions } from "./actions";

export function RoomCallouts(props: { room: RoomView; pausedUntil: number | undefined; actions: RoomActions }) {
  const { room, actions: a } = props;
  const ownerName = owner(room)?.name ?? "The owner";
  const names = room.waitingFor.map((id) => room.members.find((m) => m.id === id)?.name ?? "a member");
  const offeredTo = room.members.find((m) => m.id === room.offeringTo)?.name ?? "them";
  const callouts = [];

  if (room.isOwner && room.needsPassword) {
    callouts.push(
      <Callout
        key="password"
        tone="warn"
        actions={
          <Button size="sm" variant="primary" onClick={a.setPassword}>
            Set a new password
          </Button>
        }
        why="A backup never carries a room's password, and a room handed to you while the app was closed comes without the one you chose. Set a new one to make invites."
      >
        This room has no password yet. Set one to invite people; members already in aren't affected.
      </Callout>,
    );
  }
  if (room.catchingUp) {
    callouts.push(
      <Callout
        key="catching-up"
        tone="info"
        actions={
          room.isOwner && (
            <Button size="sm" onClick={a.stopWaiting}>
              Stop waiting…
            </Button>
          )
        }
        why={
          room.isOwner
            ? "Your room came back from a backup and may be older than the room is now. Any member away may hold the newest changes, so it waits for each of them before it signs anything."
            : undefined
        }
      >
        {room.isOwner
          ? `Restored from a backup: catching up with the room. Until ${namesListed(names)} ${names.length === 1 ? "has" : "have"} been online, nobody can be invited, let in or banned.`
          : "Restored from a backup: catching up with the room until another member is online."}
      </Callout>,
    );
  }
  if (room.offeringTo != null) {
    callouts.push(
      <Callout
        key="offering"
        tone="accent"
        actions={
          <Button size="sm" onClick={a.withdraw}>
            Withdraw the offer
          </Button>
        }
      >
        You offered the room to {offeredTo}. Waiting for them to accept…
      </Callout>,
    );
  }
  if (room.offered === false) {
    callouts.push(
      <Callout
        key="offered"
        tone="accent"
        actions={
          <>
            <Button size="sm" variant="primary" onClick={a.accept}>
              Accept…
            </Button>
            <Button size="sm" onClick={a.decline}>
              Decline
            </Button>
          </>
        }
      >
        {ownerName} offers you the room: you'd let people in, ban, and close it.
      </Callout>,
    );
  }
  if (room.offered === true) {
    callouts.push(
      <Callout key="accepted" tone="accent">
        You accepted {ownerName}'s offer. The room becomes yours as soon as {ownerName} hands it over; until then you
        can't leave it.
      </Callout>,
    );
  }
  if (props.pausedUntil != null) callouts.push(<PausedCallout key="paused" until={props.pausedUntil} />);

  if (callouts.length === 0) return null;
  return <div className="callouts">{callouts}</div>;
}

function PausedCallout(props: { until: number }) {
  const [now, setNow] = useState(Date.now());
  useEffect(() => {
    const timer = window.setInterval(() => setNow(Date.now()), 1000);
    return () => window.clearInterval(timer);
  }, []);
  const left = Math.ceil((props.until - now) / 1000);
  if (left <= 0) return null;
  return (
    <Callout tone="warn">
      Too many wrong passwords: joining is paused for <span className="tabular">{duration(left)}</span>.
    </Callout>
  );
}
