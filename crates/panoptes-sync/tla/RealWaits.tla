------------------------------ MODULE RealWaits ------------------------------
(* The same waits in a normal build, with recv_timeout until a deadline or  *)
(* a limit. A wait returns a message once one is queued, Closed once the     *)
(* sender is gone, and times out only while nothing is queued. With time     *)
(* abstract, the deadline or the limit can pass at any such point.           *)
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

StartTimer == pc = "idle" /\ pc' = "timer_wait" /\ UNCHANGED <<queue, senders, sent, res>>
EndTimer == pc = "timer_wait" /\ Take(Result("timer")) /\ pc' = "idle"
            /\ UNCHANGED <<senders, sent>>
StartReply == pc = "idle" /\ pc' = "reply_wait" /\ UNCHANGED <<queue, senders, sent, res>>
EndReply == pc = "reply_wait" /\ Take(Result("timeout")) /\ pc' = "idle"
            /\ UNCHANGED <<senders, sent>>

Next == Send \/ DropSender \/ StartTimer \/ EndTimer \/ StartReply \/ EndReply
Spec == Init /\ [][Next]_vars

\* Every real behavior is one that loom explores: a real wait maps to the
\* loom wait at the step where it returns.
LoomPc == IF pc = "timer_wait" \/ (pc = "reply_wait" /\ ReplyMode = "try")
            THEN "idle" ELSE pc
Loom == INSTANCE LoomWaits WITH pc <- LoomPc
RefinesLoom == Loom!Spec
=============================================================================
