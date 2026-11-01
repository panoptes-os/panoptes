------------------------------ MODULE LoomWaits ------------------------------
(* panoptes-sync's waits under --cfg panoptes_loom, over std's channel with  *)
(* each operation atomic. One sender thread holds one Sender; the receiver   *)
(* calls recv_timer or recv_reply. Time is abstract: a model's durations are *)
(* zero and now() stands still.                                              *)
(*                                                                           *)
(* ReplyMode "try" is the facade: recv_reply fails at once when no reply is  *)
(* queued, as recv_timer fires. "block" is a recv_reply that waits with no   *)
(* limit; RealWaits shows that it misses the timeout.                        *)
EXTENDS Naturals, Sequences
CONSTANTS MaxSends, ReplyMode
VARIABLES queue, senders, sent, pc, res
vars == <<queue, senders, sent, pc, res>>

Init == queue = <<>> /\ senders = 1 /\ sent = 0 /\ pc = "idle" /\ res = <<"none">>

Send == /\ senders = 1 /\ sent < MaxSends
        /\ queue' = Append(queue, sent + 1) /\ sent' = sent + 1
        /\ UNCHANGED <<senders, pc, res>>
DropSender == senders = 1 /\ senders' = 0 /\ UNCHANGED <<queue, sent, pc, res>>

Take(r) == /\ res' = r /\ queue' = IF queue # <<>> THEN Tail(queue) ELSE queue
Result(timeout) ==
  IF queue # <<>> THEN <<"ok", Head(queue)>>
  ELSE IF senders = 0 THEN <<"closed">> ELSE <<timeout>>

\* recv_timer: try_recv, so the timer fires whenever nothing is queued.
RecvTimer == pc = "idle" /\ Take(Result("timer")) /\ UNCHANGED <<senders, sent, pc>>

\* recv_reply, "try": the same, with the timeout error.
RecvReply == ReplyMode = "try" /\ pc = "idle" /\ Take(Result("timeout"))
             /\ UNCHANGED <<senders, sent, pc>>

\* recv_reply, "block": a recv with no limit.
StartReply == ReplyMode = "block" /\ pc = "idle" /\ pc' = "reply_wait"
              /\ UNCHANGED <<queue, senders, sent, res>>
EndReply ==
  /\ ReplyMode = "block" /\ pc = "reply_wait" /\ (queue # <<>> \/ senders = 0)
  /\ Take(Result("unreachable")) /\ pc' = "idle" /\ UNCHANGED <<senders, sent>>

Next == Send \/ DropSender \/ RecvTimer \/ RecvReply \/ StartReply \/ EndReply
Spec == Init /\ [][Next]_vars
=============================================================================
