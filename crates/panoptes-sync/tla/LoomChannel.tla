----------------------------- MODULE LoomChannel -----------------------------
(* panoptes-sync's channel under --cfg panoptes_loom, the code between the   *)
(* tla markers in ../src/lib.rs (mod loom_channel): one Mutex<State>        *)
(* and one Condvar `ready`. Each step below is one critical section or one   *)
(* lock or condvar operation. The condvar may wake its waiter spuriously.    *)
(* Bug plants a known defect, to show that the checks catch it.              *)
EXTENDS Naturals, Sequences, FiniteSets

CONSTANTS Senders, R, MaxSends, MaxClones, None, Bug

Threads == Senders \cup {R}

VARIABLES queue, senders, rxAlive, handles, sent, clones, res,
          lock, waiting, pc

chan == <<queue, senders, rxAlive, handles, sent, clones, res>>
vars == <<chan, lock, waiting, pc>>

RecvPcs == {"recv_lock", "recv_check", "recv_wait", "recv_relock", "recv_after"}

TypeOK ==
  /\ lock \in Threads \cup {None}
  /\ waiting \subseteq {R}

\* The count matches the handles that threads hold.
RECURSIVE Sum(_, _)
Sum(f, S) == IF S = {} THEN 0 ELSE LET x == CHOOSE x \in S : TRUE
                                   IN f[x] + Sum(f, S \ {x})
Consistent == senders = Sum(handles, Senders)

Init ==
  /\ queue = <<>>
  /\ handles = [t \in Senders |-> 1]
  /\ senders = Cardinality(Senders)
  /\ rxAlive = TRUE
  /\ sent = [t \in Senders |-> 0]
  /\ clones = 0
  /\ res = [t \in Threads |-> <<"none">>]
  /\ lock = None
  /\ waiting = {}
  /\ pc = [t \in Threads |-> "idle"]

Goto(t, l) == pc' = [pc EXCEPT ![t] = l]

Acquire(t, from, to) ==
  /\ pc[t] = from /\ lock = None
  /\ lock' = t /\ Goto(t, to)
  /\ UNCHANGED <<chan, waiting>>

Release(t, from, to) ==
  /\ pc[t] = from /\ lock = t
  /\ lock' = None /\ Goto(t, to)
  /\ UNCHANGED <<chan, waiting>>

\* ready.notify_one(), by t, which then moves to `to`. Only R waits.
NotifyOne(t, to) ==
  IF R \in waiting
    THEN /\ waiting' = waiting \ {R}
         /\ pc' = [pc EXCEPT ![t] = to, ![R] = "recv_relock"]
    ELSE /\ UNCHANGED waiting
         /\ Goto(t, to)

(* Sender::send *)
StartSend(t) ==
  /\ pc[t] = "idle" /\ handles[t] > 0 /\ sent[t] < MaxSends
  /\ Goto(t, "send_lock") /\ UNCHANGED <<chan, lock, waiting>>
SendBody(t) ==
  /\ pc[t] = "send_body" /\ lock = t
  /\ sent' = [sent EXCEPT ![t] = @ + 1]
  /\ IF rxAlive
       THEN /\ queue' = Append(queue, <<t, sent[t] + 1>>)
            /\ res' = [res EXCEPT ![t] = <<"sent">>]
            /\ Goto(t, IF Bug = "no_notify_on_send" THEN "send_unlock"
                                                    ELSE "send_notify")
       ELSE /\ UNCHANGED queue
            /\ res' = [res EXCEPT ![t] = <<"senderr">>]
            /\ Goto(t, "send_unlock")
  /\ UNCHANGED <<senders, rxAlive, handles, clones, lock, waiting>>
SendNotify(t) ==
  /\ pc[t] = "send_notify" /\ lock = t
  /\ NotifyOne(t, "send_unlock") /\ UNCHANGED <<chan, lock>>

(* Sender::clone *)
StartClone(t) ==
  /\ pc[t] = "idle" /\ handles[t] > 0 /\ clones < MaxClones
  /\ Goto(t, "clone_lock") /\ UNCHANGED <<chan, lock, waiting>>
CloneBody(t) ==
  /\ pc[t] = "clone_body" /\ lock = t
  /\ handles' = [handles EXCEPT ![t] = @ + 1]
  /\ senders' = senders + 1
  /\ clones' = clones + 1
  /\ Goto(t, "clone_unlock")
  /\ UNCHANGED <<queue, rxAlive, sent, res, lock, waiting>>

(* Drop for Sender: the last one wakes the receiver. *)
StartDrop(t) ==
  /\ pc[t] = "idle" /\ handles[t] > 0
  /\ Goto(t, "drop_lock") /\ UNCHANGED <<chan, lock, waiting>>
DropBody(t) ==
  /\ pc[t] = "drop_body" /\ lock = t
  /\ handles' = [handles EXCEPT ![t] = @ - 1]
  /\ senders' = senders - 1
  /\ Goto(t, IF senders - 1 = 0 /\ Bug # "no_notify_on_last_drop"
               THEN "drop_notify" ELSE "drop_unlock")
  /\ UNCHANGED <<queue, rxAlive, sent, clones, res, lock, waiting>>
DropNotify(t) ==
  /\ pc[t] = "drop_notify" /\ lock = t
  /\ NotifyOne(t, "drop_unlock") /\ UNCHANGED <<chan, lock>>

(* Receiver::recv: loop { pop; senders == 0; wait } *)
StartRecv ==
  /\ pc[R] = "idle" /\ rxAlive
  /\ Goto(R, "recv_lock") /\ UNCHANGED <<chan, lock, waiting>>
Pop ==
  /\ res' = [res EXCEPT ![R] = <<"ok", Head(queue)>>]
  /\ queue' = Tail(queue)
Closed ==
  /\ res' = [res EXCEPT ![R] = <<"closed">>]
  /\ UNCHANGED queue
RecvReturn(Result) ==
  /\ Result /\ Goto(R, "recv_unlock") /\ UNCHANGED <<lock, waiting>>
\* ready.wait(s): release the lock and wait, in one step.
RecvWait ==
  /\ lock' = None /\ waiting' = waiting \cup {R}
  /\ Goto(R, "recv_wait") /\ UNCHANGED <<queue, res>>
RecvCheck ==
  /\ pc[R] = "recv_check" /\ lock = R
  /\ IF Bug = "closed_first"
       THEN (IF senders = 0 THEN RecvReturn(Closed)
             ELSE IF queue # <<>> THEN RecvReturn(Pop)
             ELSE RecvWait)
       ELSE (IF queue # <<>> THEN RecvReturn(Pop)
             ELSE IF senders = 0 THEN RecvReturn(Closed)
             ELSE RecvWait)
  /\ UNCHANGED <<senders, rxAlive, handles, sent, clones>>
\* A wakeup that no notify caused.
Spurious ==
  /\ pc[R] = "recv_wait" /\ R \in waiting
  /\ waiting' = waiting \ {R}
  /\ Goto(R, "recv_relock")
  /\ UNCHANGED <<chan, lock>>
\* With Bug = "no_recheck", recv uses `if` where the code uses `loop`.
RecvAfter ==
  /\ pc[R] = "recv_after" /\ lock = R
  /\ IF queue # <<>> THEN Pop ELSE Closed
  /\ Goto(R, "recv_unlock")
  /\ UNCHANGED <<senders, rxAlive, handles, sent, clones, lock, waiting>>

(* Receiver::try_recv *)
StartTry ==
  /\ pc[R] = "idle" /\ rxAlive
  /\ Goto(R, "try_lock") /\ UNCHANGED <<chan, lock, waiting>>
TryBody ==
  /\ pc[R] = "try_body" /\ lock = R
  /\ IF queue # <<>>
       THEN Pop
       ELSE /\ res' = [res EXCEPT ![R] = IF senders = 0 THEN <<"disc">> ELSE <<"empty">>]
            /\ UNCHANGED queue
  /\ Goto(R, "try_unlock")
  /\ UNCHANGED <<senders, rxAlive, handles, sent, clones, lock, waiting>>

(* Drop for Receiver *)
StartDropRx ==
  /\ pc[R] = "idle" /\ rxAlive
  /\ Goto(R, "droprx_lock") /\ UNCHANGED <<chan, lock, waiting>>
DropRxBody ==
  /\ pc[R] = "droprx_body" /\ lock = R
  /\ rxAlive' = FALSE
  /\ queue' = <<>>
  /\ Goto(R, "droprx_unlock")
  /\ UNCHANGED <<senders, handles, sent, clones, res, lock, waiting>>

\* Steps a thread takes once it has started an operation.
SenderSteps(t) ==
  \/ Acquire(t, "send_lock", "send_body") \/ SendBody(t) \/ SendNotify(t)
  \/ Release(t, "send_unlock", "idle")
  \/ Acquire(t, "clone_lock", "clone_body") \/ CloneBody(t)
  \/ Release(t, "clone_unlock", "idle")
  \/ Acquire(t, "drop_lock", "drop_body") \/ DropBody(t) \/ DropNotify(t)
  \/ Release(t, "drop_unlock", "idle")

ReceiverSteps ==
  \/ Acquire(R, "recv_lock", "recv_check") \/ RecvCheck
  \/ Acquire(R, "recv_relock", IF Bug = "no_recheck" THEN "recv_after"
                                                     ELSE "recv_check")
  \/ RecvAfter \/ Release(R, "recv_unlock", "idle")
  \/ Acquire(R, "try_lock", "try_body") \/ TryBody
  \/ Release(R, "try_unlock", "idle")
  \/ Acquire(R, "droprx_lock", "droprx_body") \/ DropRxBody
  \/ Release(R, "droprx_unlock", "dead")

\* Starting an operation is the client's choice, so it has no fairness.
Next ==
  \/ \E t \in Senders :
       StartSend(t) \/ StartClone(t) \/ StartDrop(t) \/ SenderSteps(t)
  \/ StartRecv \/ StartTry \/ StartDropRx \/ ReceiverSteps \/ Spurious

Fairness ==
  /\ \A t \in Senders : WF_vars(SenderSteps(t))
  /\ WF_vars(ReceiverSteps)

Spec == Init /\ [][Next]_vars /\ Fairness

(* Checks *)
Mutex == \A t \in Threads :
  pc[t] \in {"send_body", "send_notify", "send_unlock", "clone_body",
             "clone_unlock", "drop_body", "drop_notify", "drop_unlock",
             "recv_check", "recv_after", "recv_unlock", "try_body",
             "try_unlock", "droprx_body", "droprx_unlock"} => lock = t

\* The channel is std's mpsc: every step maps to one of its operations.
Std == INSTANCE Mpsc
RefinesStd == Std!Spec

\* No lost wakeup: a recv that can return, does.
RecvReturns ==
  (pc[R] \in RecvPcs /\ (queue # <<>> \/ senders = 0))
    ~> (pc[R] \notin RecvPcs)
=============================================================================
