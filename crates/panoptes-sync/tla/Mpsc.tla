-------------------------------- MODULE Mpsc --------------------------------
(* std's mpsc contract, with each operation atomic. One receiver thread R;  *)
(* each sender thread starts with one Sender and may clone or drop it.       *)
(* Messages are <<thread, n>>, so queue equality also checks per-sender      *)
(* order. res[t] is the result of t's last operation.                        *)
EXTENDS Naturals, Sequences, FiniteSets

CONSTANTS Senders, R, MaxSends, MaxClones

Threads == Senders \cup {R}

VARIABLES queue, senders, rxAlive, handles, sent, clones, res

absvars == <<queue, senders, rxAlive, handles, sent, clones, res>>

Init ==
  /\ queue = <<>>
  /\ handles = [t \in Senders |-> 1]
  /\ senders = Cardinality(Senders)
  /\ rxAlive = TRUE
  /\ sent = [t \in Senders |-> 0]
  /\ clones = 0
  /\ res = [t \in Threads |-> <<"none">>]

\* send: Err once the receiver is gone; never blocks.
Send(t) ==
  /\ handles[t] > 0
  /\ sent[t] < MaxSends
  /\ sent' = [sent EXCEPT ![t] = @ + 1]
  /\ IF rxAlive
       THEN /\ queue' = Append(queue, <<t, sent[t] + 1>>)
            /\ res' = [res EXCEPT ![t] = <<"sent">>]
       ELSE /\ UNCHANGED queue
            /\ res' = [res EXCEPT ![t] = <<"senderr">>]
  /\ UNCHANGED <<senders, rxAlive, handles, clones>>

Clone(t) ==
  /\ handles[t] > 0
  /\ handles' = [handles EXCEPT ![t] = @ + 1]
  /\ senders' = senders + 1
  /\ clones' = clones + 1
  /\ UNCHANGED <<queue, rxAlive, sent, res>>

DropSender(t) ==
  /\ handles[t] > 0
  /\ handles' = [handles EXCEPT ![t] = @ - 1]
  /\ senders' = senders - 1
  /\ UNCHANGED <<queue, rxAlive, sent, clones, res>>

\* recv: queued messages first, then Err once every sender is gone. It
\* blocks, so it is enabled only when it can return.
Recv ==
  /\ rxAlive
  /\ queue # <<>> \/ senders = 0
  /\ IF queue # <<>>
       THEN /\ res' = [res EXCEPT ![R] = <<"ok", Head(queue)>>]
            /\ queue' = Tail(queue)
       ELSE /\ res' = [res EXCEPT ![R] = <<"closed">>]
            /\ UNCHANGED queue
  /\ UNCHANGED <<senders, rxAlive, handles, sent, clones>>

TryRecv ==
  /\ rxAlive
  /\ IF queue # <<>>
       THEN /\ res' = [res EXCEPT ![R] = <<"ok", Head(queue)>>]
            /\ queue' = Tail(queue)
       ELSE /\ res' = [res EXCEPT ![R] = IF senders = 0 THEN <<"disc">> ELSE <<"empty">>]
            /\ UNCHANGED queue
  /\ UNCHANGED <<senders, rxAlive, handles, sent, clones>>

\* Dropping the receiver drops the queued messages.
DropRx ==
  /\ rxAlive
  /\ rxAlive' = FALSE
  /\ queue' = <<>>
  /\ UNCHANGED <<senders, handles, sent, clones, res>>

Next ==
  \/ \E t \in Senders : Send(t) \/ Clone(t) \/ DropSender(t)
  \/ Recv \/ TryRecv \/ DropRx

Spec == Init /\ [][Next]_absvars
=============================================================================
