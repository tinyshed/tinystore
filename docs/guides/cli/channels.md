# channels

`tinystore channels` publishes a message and listens to a channel from a
shell, with no SDK. A channel lives in the memory of the process that holds
the store, so both go through the server that holds the directory, or a
sidecar they start, and nothing is read beside it.

```sh
tinystore channels ./data publish chat-events '{"kind": "reload"}'
tinystore channels ./data subscribe chat-events --for 1m
```

## Publish a message

```sh
tinystore channels ./data publish chat-events '{"kind": "reload"}'
tinystore channels ./data publish chat-events '{"seen": 7}' --under room-42
```

```text
published to chat-events under room-42
```

- A message is JSON. `-` reads it from stdin.
- `--under room-42` sends it to a branch of the channel, its owners written
  as `42/devices`.
- Every subscription listening now receives it, and no one else ever will: a
  message published while no one listens is gone, as the
  [channels guide](../channels/README.md) says.
- It is sent once. A message whose answer was lost may have been delivered,
  and sending it again would deliver it twice.

## Listen to a channel

```sh
tinystore channels ./data subscribe chat-events --for 1m
```

```text
tinystore channels subscribe: listening on chat-events
{"kind":"reload"}
room-42  {"seen":7}
```

- stderr says when it listens. A message published before that line is not
  received.
- Each message is a line on stdout, after its branch when it has one.
  `--json` gives `{"under": […], "value": …}` a line.
- `--under room-42` listens to one branch, and `--recursive` to every branch
  below it too.
- `--for 1m` stops after a minute. Without it, it listens until stopped with
  Ctrl+C.
- A subscription that falls too far behind loses messages, and stderr says
  how many.

## See also

- [channels](../channels/README.md): channels from a program, branches, and a
  transaction's messages at its commit.
