#!/usr/bin/env python3
# Generates trans/trans.ent (Enmerkar hieroglyph source). Convenience only —
# the artifact is hand-authored Enmerkar; this just avoids glyph-counting typos.
#
# NOTE: This generator is outdated. It targets v9 dense encoding with removed
# opcodes (JMP/JZ/JE/IDX/SETI). The hand-written trans/trans.uf is the
# authoritative source (v11 text encoding). Do not regenerate without first
# updating this script to the v11 opcode set and ^-prefixed variable syntax.
import sys

def O(i): return chr(0x13000 + i)   # opcode glyph by index
def V(i): return chr(0x13080 + i)   # variable slot glyph

LIT,DUP,OVR,DRP,SWP,PICK,ADD,SUB,MUL,AND,SHR,INC,DEC,JMP,JZ,JE,FOR,CALL,RET = range(19)
IDX,SETI,MACRO = 24,25,28
SETV,GETV,STR,CAT,FMT = 33,34,35,36,37
BUFCOPY,ADDR,LOADX,STOREX = 40,41,42,43
ARR = 23
IMPORT,EXPORT,EXTERN = 51,52,53

def sv(i): return V(i)+O(SETV)
def gv(i): return V(i)+O(GETV)
def lit(n): return O(LIT)+str(n)

# register map (globals)
SRC,SRCDAT,N,POS = 0,1,2,3
TT,TVL,NTOK,PI = 4,5,6,7
NTAB,NDAT,NUSED,NOFF,NLENG,NNAMES = 8,9,10,11,12,13
NEXTVAR,LBL,STDOUT = 14,15,16
SCR,CT,VN,VS,VCNT = 17,18,19,20,21
START,LENG,RES,FOUND = 22,23,24,25
PCNT,PVARS = 26,27
SCR2,LSTK,LCNT,ISV = 28,29,30,31

out = []
w = out.append

w("; trans.uf - C subset -> uFlux transpiler, written in uFlux\n")
w("; regs: 0 src 1 srcdat 2 n 3 pos | 4 ttypes 5 tvals 6 ntoks 7 pi | 8 ntab 9 ndat 10 nused 11 noff 12 nleng 13 nnames\n")
w("; 14 nextvar 15 lbl 16 stdout 17 scr 18 ct 19 vn 20 vs 21 vcnt | 22 start 23 len 24 res 25 found 26 pcnt 27 pvars 28 scr2 29 lstk 30 lcnt\n")
w("; tok types: 0 EOF 1 ID 2 NUM 3 STR | 100 int 101 return 102 if 103 else 104 while | 200== 201!= 202<= 203>= 204&& 205|| | else ASCII\n")

# imports
for sig in ['c"fopen"(ptr,ptr)->ptr','c"fseek"(ptr,int,int)->int','c"ftell"(ptr)->int',
            'c"fread"(ptr,int,int,ptr)->int','c"fclose"(ptr)->int','c"putchar"(int)->int',
            'c"fputs"(ptr,ptr)->int','c"puts"(ptr)->int','c"strncmp"(ptr,ptr,int)->int',
            'c"exit"(int)->void']:
    w(O(IMPORT)+sig)
for x in ['"stdout"','"nkr_argc"','"nkr_argv"']:
    w(O(EXTERN)+x)
w('\n')

# macros
w(O(MACRO)+'s63{'+O(SHR)*63+'}')
w(O(MACRO)+'pc{'+gv(POS)+gv(SRC)+O(SWP)+O(IDX)+'}')
w(O(MACRO)+'cl{'+' pc '+gv(CT)+O(SWP)+O(IDX)+'}')
w(O(MACRO)+'tt{'+gv(PI)+gv(TT)+O(SWP)+O(IDX)+'}')
w(O(MACRO)+'tv{'+gv(PI)+gv(TVL)+O(SWP)+O(IDX)+'}')
w(O(MACRO)+'tt2{'+gv(PI)+lit(1)+O(ADD)+gv(TT)+O(SWP)+O(IDX)+'}')
w(O(MACRO)+'adv{'+gv(PI)+O(INC)+sv(PI)+'}')
w(O(MACRO)+'nx{'+gv(POS)+O(INC)+sv(POS)+'}')
w('\n')

w(O(CALL)+'main'+O(RET)+'\n')

# ---------- emitters ----------
# eg: opcode glyph k (k<64) + trailing space
w('eg:'+sv(SCR)+lit(240)+O(CALL)+'putchar'+O(DRP)+lit(147)+O(CALL)+'putchar'+O(DRP)
  +lit(128)+O(CALL)+'putchar'+O(DRP)+lit(128)+gv(SCR)+O(ADD)+O(CALL)+'putchar'+O(DRP)
  +lit(32)+O(CALL)+'putchar'+O(DRP)+O(RET)+'\n')
# evg: variable slot glyph k (k<64) + trailing space
w('evg:'+sv(SCR)+lit(240)+O(CALL)+'putchar'+O(DRP)+lit(147)+O(CALL)+'putchar'+O(DRP)
  +lit(130)+O(CALL)+'putchar'+O(DRP)+lit(128)+gv(SCR)+O(ADD)+O(CALL)+'putchar'+O(DRP)
  +lit(32)+O(CALL)+'putchar'+O(DRP)+O(RET)+'\n')
# estr: cstr ->
w('estr:'+gv(STDOUT)+O(CALL)+'fputs'+O(DRP)+O(RET)+'\n')
# eint: n ->
w('eint:'+O(STR)+'"%lld"'+O(FMT)+O(CALL)+'estr'+O(RET)+'\n')
# ename: nameid ->  (emits identifier bytes)
w('ename:'+sv(SCR)+gv(SCR)+gv(NOFF)+O(SWP)+O(IDX)+sv(START)
  +gv(SCR)+gv(NLENG)+O(SWP)+O(IDX)+sv(LENG)
  +gv(LENG)+O(ADDR)+'enb'+O(FOR)+O(RET)+'\n')
w('enb:'+gv(START)+O(ADD)+gv(NTAB)+O(SWP)+O(IDX)+O(CALL)+'putchar'+O(DRP)+O(RET)+'\n')
# elb: n ->  emit "Ln: "   elj: n -> emit "Ln "
w('elb:'+sv(SCR)+O(STR)+'"L"'+O(CALL)+'estr'+gv(SCR)+O(CALL)+'eint'
  +lit(58)+O(CALL)+'putchar'+O(DRP)+lit(32)+O(CALL)+'putchar'+O(DRP)+O(RET)+'\n')
w('elj:'+sv(SCR)+O(STR)+'"L"'+O(CALL)+'estr'+gv(SCR)+O(CALL)+'eint'
  +lit(32)+O(CALL)+'putchar'+O(DRP)+O(RET)+'\n')
# egets / esets: slot -> emit slot glyph + GETV/SETV
w( 'isvoid:'+sv(SCR)+gv(SCR)+gv(NLENG)+O(SWP)+O(IDX)+lit(4)+O(JE)+'iv4'+lit(0)+O(RET)+'\n')
w('iv4:'+gv(NDAT)+gv(SCR)+gv(NOFF)+O(SWP)+O(IDX)+O(ADD)+O(STR)+'"free"'+lit(4)+O(CALL)+'strncmp'+lit(0)+O(JE)+'ivy'+lit(0)+O(RET)+'\n')
w('ivy:'+lit(1)+O(RET)+'\n')
w('egets:'+sv(SCR)+gv(SCR)+O(CALL)+'evg'+lit(GETV)+O(CALL)+'eg'+O(RET)+'\n')
w('esets:'+sv(SCR)+gv(SCR)+O(CALL)+'evg'+lit(SETV)+O(CALL)+'eg'+O(RET)+'\n')
# die: msg ->
w('die:'+O(CALL)+'puts'+O(DRP)+lit(1)+O(CALL)+'exit'+O(RET)+'\n')
# zlt: a b -> a<b (0/1)
w('zlt:'+O(SUB)+' s63 '+O(RET)+'\n')

# ---------- lexer ----------
# tok: type val ->
w('tok:'+sv(SCR)+sv(RES)
  +gv(RES)+gv(NTOK)+gv(TT)+O(SETI)
  +gv(SCR)+gv(NTOK)+gv(TVL)+O(SETI)
  +gv(NTOK)+O(INC)+sv(NTOK)+O(RET)+'\n')
# kwm: kptr klen -> 1/0   (strncmp with srcdat+start)
w('kwm:'+sv(SCR)+sv(RES)
  +gv(RES)+gv(SRCDAT)+gv(START)+O(ADD)+gv(SCR)+O(CALL)+'strncmp'
  +lit(0)+O(JE)+'kwy'+lit(0)+O(RET)+'\n')
w('kwy:'+lit(1)+O(RET)+'\n')
# iskw: start len -> type (0 = plain ident)
w('iskw:'+sv(LENG)+sv(START))
def kw(word, tid, nxt):
    return (gv(LENG)+lit(len(word))+O(JE)+'ik_t'+word+O(JMP)+'ik_n'+word+'\n'
            +'ik_t'+word+':'+O(STR)+'"'+word+'"'+lit(len(word))+O(CALL)+'kwm'
            +lit(1)+O(JE)+'ik_y'+word+O(JMP)+'ik_n'+word+'\n'
            +'ik_y'+word+':'+lit(tid)+O(RET)+'\n'
            +'ik_n'+word+':')
w(kw('int',100,''))
w(kw('if',102,''))
w(kw('else',103,''))
w(kw('while',104,''))
w(kw('return',101,''))
w(lit(0)+O(RET)+'\n')
# intern: start len -> id
w('intern:'+sv(LENG)+sv(START)+lit(-1)+sv(FOUND)
  +gv(NNAMES)+O(ADDR)+'inb'+O(FOR)
  +gv(FOUND)+lit(1)+O(ADD)+O(JZ)+'inap'+O(JMP)+'inre'+'\n')
w('inb:'+O(DUP)+gv(NLENG)+O(SWP)+O(IDX)+gv(LENG)+O(JE)+'inck'+O(DRP)+O(RET)+'\n')
w('inck:'+sv(SCR)
  +gv(NDAT)+gv(SCR)+gv(NOFF)+O(SWP)+O(IDX)+O(ADD)
  +gv(SRCDAT)+gv(START)+O(ADD)
  +gv(LENG)+O(CALL)+'strncmp'+lit(0)+O(JE)+'inf'+O(RET)+'\n')
w('inf:'+gv(SCR)+sv(FOUND)+O(RET)+'\n')
w('inap:'
  +gv(NUSED)+gv(NNAMES)+gv(NOFF)+O(SETI)
  +gv(LENG)+gv(NNAMES)+gv(NLENG)+O(SETI)
  +gv(NDAT)+gv(NUSED)+O(ADD)+gv(SRCDAT)+gv(START)+O(ADD)+gv(LENG)+O(BUFCOPY)
  +gv(NUSED)+gv(LENG)+O(ADD)+sv(NUSED)
  +gv(NNAMES)+sv(FOUND)+gv(NNAMES)+O(INC)+sv(NNAMES)+'\n')
w('inre:'+gv(FOUND)+O(RET)+'\n')
# lx: main lex loop
w('lx:'+'\n')
w('lxl:'+' pc '+lit(0)+O(JE)+'lxd'
  +' cl '+lit(1)+O(JE)+'lxw'
  +' cl '+lit(2)+O(JE)+'lxn'
  +' cl '+lit(3)+O(JE)+'lxi'
  +' pc '+lit(34)+O(JE)+'lxs'
  +O(JMP)+'lxp'+'\n')
w('lxw:'+' nx '+O(JMP)+'lxl'+'\n')
w('lxd:'+lit(0)+lit(0)+O(CALL)+'tok'+O(RET)+'\n')
# number
w('lxn:'+lit(0)+sv(SCR)+'\n')
w('lxnl:'+' cl '+lit(2)+O(JE)+'lxnc'+O(JMP)+'lxnd'+'\n')
w('lxnc:'+gv(SCR)+lit(10)+O(MUL)+' pc '+lit(48)+O(SUB)+O(ADD)+sv(SCR)+' nx '+O(JMP)+'lxnl'+'\n')
w('lxnd:'+lit(2)+gv(SCR)+O(CALL)+'tok'+O(JMP)+'lxl'+'\n')
# ident / keyword
w('lxi:'+gv(POS)+sv(START)+'\n')
w('lxil:'+' cl '+lit(3)+O(JE)+'lxic'+' cl '+lit(2)+O(JE)+'lxic'+O(JMP)+'lxid'+'\n')
w('lxic:'+' nx '+O(JMP)+'lxil'+'\n')
w('lxid:'+gv(POS)+gv(START)+O(SUB)+sv(LENG)
  +gv(START)+gv(LENG)+O(CALL)+'iskw'+sv(RES)
  +gv(RES)+lit(0)+O(JE)+'lxii'
  +gv(RES)+lit(0)+O(CALL)+'tok'+O(JMP)+'lxl'+'\n')
w('lxii:'+gv(START)+gv(LENG)+O(CALL)+'intern'+sv(SCR)
  +lit(1)+gv(SCR)+O(CALL)+'tok'+O(JMP)+'lxl'+'\n')
# string
w('lxs:'+' nx '+gv(POS)+sv(START)+'\n')
w('lxsl:'+' pc '+lit(34)+O(JE)+'lxsd'+' pc '+lit(92)+O(JE)+'lxse'+' nx '+O(JMP)+'lxsl'+'\n')
w('lxse:'+' nx '+' '+' nx '+O(JMP)+'lxsl'+'\n')
w('lxsd:'+gv(POS)+gv(START)+O(SUB)+sv(LENG)
  +gv(START)+gv(LENG)+O(CALL)+'intern'+sv(SCR)+' nx '
  +lit(3)+gv(SCR)+O(CALL)+'tok'+O(JMP)+'lxl'+'\n')
# punct
w('lxp:'+gv(POS)+gv(SRC)+O(SWP)+O(IDX)+sv(RES)
  +gv(POS)+lit(1)+O(ADD)+gv(SRC)+O(SWP)+O(IDX)+sv(SCR)+'\n')
def two(ch1, ch2, tid, n):
    return (gv(RES)+lit(ch1)+O(JE)+f'lxt{n}a'+O(JMP)+f'lxn{n}'+'\n'
            +f'lxt{n}a:'+gv(SCR)+lit(ch2)+O(JE)+f'lxt{n}b'+O(JMP)+'lxps'+'\n'
            +f'lxt{n}b:'+' nx '+' '+' nx '+lit(tid)+lit(0)+O(CALL)+'tok'+O(JMP)+'lxl'+'\n'
            +f'lxn{n}:')
w(two(61,61,200,1))
w(two(33,61,201,2))
w(two(60,61,202,3))
w(two(62,61,203,4))
w(two(38,38,204,5))
w(two(124,124,205,6))
w('lxps:'+' nx '+gv(RES)+lit(0)+O(CALL)+'tok'+O(JMP)+'lxl'+'\n')
# ctinit: character classes  ct: 0 other 1 space 2 digit 3 alpha
w('ctinit:'
  +lit(1)+lit(32)+gv(CT)+O(SETI)
  +lit(1)+lit(9)+gv(CT)+O(SETI)
  +lit(1)+lit(10)+gv(CT)+O(SETI)
  +lit(1)+lit(13)+gv(CT)+O(SETI)
  +lit(10)+O(ADDR)+'ctd'+O(FOR)
  +lit(26)+O(ADDR)+'ctu'+O(FOR)
  +lit(26)+O(ADDR)+'ctl'+O(FOR)
  +lit(3)+lit(95)+gv(CT)+O(SETI)
  +O(RET)+'\n')
w('ctd:'+lit(48)+O(ADD)+lit(2)+O(SWP)+gv(CT)+O(SETI)+O(RET)+'\n')
w('ctu:'+lit(65)+O(ADD)+lit(3)+O(SWP)+gv(CT)+O(SETI)+O(RET)+'\n')
w('ctl:'+lit(97)+O(ADD)+lit(3)+O(SWP)+gv(CT)+O(SETI)+O(RET)+'\n')

# ---------- parser ----------
# label stack
w('lpush:'+gv(LCNT)+gv(LSTK)+O(SETI)+gv(LCNT)+O(INC)+sv(LCNT)+O(RET)+'\n')
w('lpop:'+gv(LCNT)+lit(1)+O(SUB)+sv(LCNT)+gv(LCNT)+gv(LSTK)+O(SWP)+O(IDX)+O(RET)+'\n')
w('ltop:'+gv(LCNT)+lit(1)+O(SUB)+gv(LSTK)+O(SWP)+O(IDX)+O(RET)+'\n')
w('ltop2:'+gv(LCNT)+lit(2)+O(SUB)+gv(LSTK)+O(SWP)+O(IDX)+O(RET)+'\n')
# exp: type ->
w('exp:'+sv(SCR)+' tt '+gv(SCR)+O(JE)+'exo'
  +O(STR)+'"parse error"'+O(CALL)+'die'+'\n')
w('exo:'+' adv '+O(RET)+'\n')
# newvar: nameid -> slot
w('newvar:'+sv(SCR)
  +gv(SCR)+gv(VCNT)+gv(VN)+O(SETI)
  +gv(NEXTVAR)+gv(VCNT)+gv(VS)+O(SETI)
  +gv(VCNT)+O(INC)+sv(VCNT)
  +gv(NEXTVAR)+sv(SCR)+gv(NEXTVAR)+O(INC)+sv(NEXTVAR)
  +gv(SCR)+O(RET)+'\n')
# findvar: nameid -> slot
w('findvar:'+sv(SCR)+lit(-1)+sv(FOUND)
  +gv(VCNT)+O(ADDR)+'fvb'+O(FOR)
  +gv(FOUND)+lit(1)+O(ADD)+O(JZ)+'fvd'
  +gv(FOUND)+O(RET)+'\n')
w('fvb:'+O(DUP)+gv(VN)+O(SWP)+O(IDX)+gv(SCR)+O(JE)+'fvm'+O(DRP)+O(RET)+'\n')
w('fvm:'+gv(VS)+O(SWP)+O(IDX)+sv(FOUND)+O(RET)+'\n')
w('fvd:'+O(STR)+'"undefined variable"'+O(CALL)+'die'+'\n')
# pprog
w('pprog:'+'\n')
w('ppl:'+' tt '+lit(0)+O(JE)+'ppd'+O(CALL)+'pfunc'+O(JMP)+'ppl'+'\n')
w('ppd:'+O(RET)+'\n')
# pfunc
w('pfunc:'+lit(0)+sv(VCNT)+lit(100)+O(CALL)+'exp'
  +lit(10)+O(CALL)+'putchar'+O(DRP)+' tv '+O(CALL)+'ename'+lit(58)+O(CALL)+'putchar'+O(DRP)
  +lit(32)+O(CALL)+'putchar'+O(DRP)+' adv '
  +lit(40)+O(CALL)+'exp'
  +lit(0)+sv(PCNT)+'\n')
w('pfp:'+' tt '+lit(41)+O(JE)+'pfq'
  +lit(100)+O(CALL)+'exp'
  +' tv '+O(CALL)+'newvar'+gv(PCNT)+gv(PVARS)+O(SETI)
  +gv(PCNT)+O(INC)+sv(PCNT)+' adv '
  +' tt '+lit(44)+O(JE)+'pfcm'+O(JMP)+'pfp'+'\n')
w('pfcm:'+' adv '+O(JMP)+'pfp'+'\n')
w('pfq:'+lit(41)+O(CALL)+'exp'
  +gv(PCNT)+O(ADDR)+'pfb'+O(FOR)
  +lit(123)+O(CALL)+'exp'+'\n')
w('psb2:'+' tt '+lit(125)+O(JE)+'pfe'+O(CALL)+'pstmt'+O(JMP)+'psb2'+'\n')
w('pfe:'+' adv '+lit(RET)+O(CALL)+'eg'+O(RET)+'\n')
w('pfb:'+gv(PCNT)+lit(1)+O(SUB)+O(SWP)+O(SUB)+gv(PVARS)+O(SWP)+O(IDX)+O(CALL)+'esets'+O(RET)+'\n')
# pstmt
w('pstmt:'+' tt '+lit(100)+O(JE)+'psd'
  +' tt '+lit(101)+O(JE)+'psr'
  +' tt '+lit(102)+O(JE)+'psi'
  +' tt '+lit(104)+O(JE)+'psw'
  +' tt '+lit(123)+O(JE)+'psb'
  +' tt '+lit(59)+O(JE)+'pss'
  +O(JMP)+'pse'+'\n')
# decl: int x [= expr] ;
w('psd:'+' adv '+' tv '+O(CALL)+'newvar'+sv(SCR2)+' adv '
  +' tt '+lit(61)+O(JE)+'psdi'+lit(59)+O(CALL)+'exp'+O(RET)+'\n')
w('psdi:'+' adv '+O(CALL)+'por'+gv(SCR2)+O(CALL)+'esets'+lit(59)+O(CALL)+'exp'+O(RET)+'\n')
# return
w('psr:'+' adv '+O(CALL)+'por'+lit(RET)+O(CALL)+'eg'+lit(59)+O(CALL)+'exp'+O(RET)+'\n')
# if / else
w('psi:'+' adv '+lit(40)+O(CALL)+'exp'+O(CALL)+'por'+lit(41)+O(CALL)+'exp'
  +gv(LBL)+O(CALL)+'lpush'+gv(LBL)+O(INC)+sv(LBL)
  +lit(JZ)+O(CALL)+'eg'+O(CALL)+'ltop'+O(CALL)+'elj'
  +O(CALL)+'pstmt'
  +' tt '+lit(103)+O(JE)+'psie'
  +O(CALL)+'lpop'+O(CALL)+'elb'+O(RET)+'\n')
w('psie:'+' adv '
  +gv(LBL)+O(CALL)+'lpush'+gv(LBL)+O(INC)+sv(LBL)
  +lit(JMP)+O(CALL)+'eg'+O(CALL)+'ltop'+O(CALL)+'elj'
  +O(CALL)+'ltop2'+O(CALL)+'elb'
  +O(CALL)+'pstmt'
  +O(CALL)+'lpop'+O(CALL)+'elb'
  +O(CALL)+'lpop'+O(DRP)+O(RET)+'\n')
# while
w('psw:'+' adv '
  +gv(LBL)+O(CALL)+'lpush'+gv(LBL)+O(INC)+sv(LBL)
  +gv(LBL)+O(CALL)+'lpush'+gv(LBL)+O(INC)+sv(LBL)
  +O(CALL)+'ltop2'+O(CALL)+'elb'
  +lit(40)+O(CALL)+'exp'+O(CALL)+'por'+lit(41)+O(CALL)+'exp'
  +lit(JZ)+O(CALL)+'eg'+O(CALL)+'ltop'+O(CALL)+'elj'
  +O(CALL)+'pstmt'
  +lit(JMP)+O(CALL)+'eg'+O(CALL)+'ltop2'+O(CALL)+'elj'
  +O(CALL)+'lpop'+O(CALL)+'elb'
  +O(CALL)+'lpop'+O(DRP)+O(RET)+'\n')
# block
w('psb:'+' adv '+'\n')
w('psbl:'+' tt '+lit(125)+O(JE)+'psbd'+O(CALL)+'pstmt'+O(JMP)+'psbl'+'\n')
w('psbd:'+' adv '+O(RET)+'\n')
w('pss:'+' adv '+O(RET)+'\n')
# expression statement / assignment
w('pse:'+' tt '+lit(1)+O(JE)+'psei'+O(JMP)+'psex'+'\n')
w('psei:'+' tt2 '+lit(61)+O(JE)+'psas'+O(JMP)+'psex'+'\n')
w('psas:'+' tv '+O(CALL)+'findvar'+sv(SCR2)+' adv '+' '+' adv '+O(CALL)+'por'
  +gv(SCR2)+O(CALL)+'esets'+lit(59)+O(CALL)+'exp'+O(RET)+'\n')
w('psex:'+lit(0)+sv(ISV)+O(CALL)+'por'+gv(ISV)+lit(1)+O(JE)+'pskv'+lit(DRP)+O(CALL)+'eg'+' pskv:'+lit(59)+O(CALL)+'exp'+O(RET)+'\n')
# expression levels
w('por:'+O(CALL)+'pand'+'\n')
w('porl:'+' tt '+lit(205)+O(JE)+'porc'+O(RET)+'\n')
w('porc:'+' adv '+O(CALL)+'pand'+lit(CALL)+O(CALL)+'eg'+O(STR)+'"lor"'+O(CALL)+'estr'
  +lit(32)+O(CALL)+'putchar'+O(DRP)+O(JMP)+'porl'+'\n')
w('pand:'+O(CALL)+'peq'+'\n')
w('pandl:'+' tt '+lit(204)+O(JE)+'pandc'+O(RET)+'\n')
w('pandc:'+' adv '+O(CALL)+'peq'+lit(CALL)+O(CALL)+'eg'+O(STR)+'"land"'+O(CALL)+'estr'
  +lit(32)+O(CALL)+'putchar'+O(DRP)+O(JMP)+'pandl'+'\n')
w('peq:'+O(CALL)+'prel'+'\n')
w('peql:'+' tt '+lit(200)+O(JE)+'peq1'+' tt '+lit(201)+O(JE)+'peq2'+O(RET)+'\n')
w('peq1:'+' adv '+O(CALL)+'prel'+lit(CALL)+O(CALL)+'eg'+O(STR)+'"eq"'+O(CALL)+'estr'
  +lit(32)+O(CALL)+'putchar'+O(DRP)+O(JMP)+'peql'+'\n')
w('peq2:'+' adv '+O(CALL)+'prel'+lit(CALL)+O(CALL)+'eg'+O(STR)+'"ne"'+O(CALL)+'estr'
  +lit(32)+O(CALL)+'putchar'+O(DRP)+O(JMP)+'peql'+'\n')
w('prel:'+O(CALL)+'padd'+'\n')
w('prell:'+' tt '+lit(60)+O(JE)+'prel1'+' tt '+lit(202)+O(JE)+'prel2'
  +' tt '+lit(62)+O(JE)+'prel3'+' tt '+lit(203)+O(JE)+'prel4'+O(RET)+'\n')
for n,(op,nm) in enumerate([('lt','lt'),('le','le'),('gt','gt'),('ge','ge')]):
    w(f'prel{n+1}:'+' adv '+O(CALL)+'padd'+lit(CALL)+O(CALL)+'eg'+O(STR)+f'"{nm}"'+O(CALL)+'estr'
      +lit(32)+O(CALL)+'putchar'+O(DRP)+O(JMP)+'prell'+'\n')
w('padd:'+O(CALL)+'pmul'+'\n')
w('paddl:'+' tt '+lit(43)+O(JE)+'padd1'+' tt '+lit(45)+O(JE)+'padd2'+O(RET)+'\n')
w('padd1:'+' adv '+O(CALL)+'pmul'+lit(ADD)+O(CALL)+'eg'+O(JMP)+'paddl'+'\n')
w('padd2:'+' adv '+O(CALL)+'pmul'+lit(SUB)+O(CALL)+'eg'+O(JMP)+'paddl'+'\n')
w('pmul:'+O(CALL)+'punary'+'\n')
w('pmull:'+' tt '+lit(42)+O(JE)+'pmul1'+' tt '+lit(47)+O(JE)+'pmul2'+' tt '+lit(37)+O(JE)+'pmul2'+O(RET)+'\n')
w('pmul1:'+' adv '+O(CALL)+'punary'+lit(MUL)+O(CALL)+'eg'+O(JMP)+'pmull'+'\n')
w('pmul2:'+O(STR)+'"no division/modulo in subset"'+O(CALL)+'die'+'\n')
w('punary:'+' tt '+lit(45)+O(JE)+'pun1'+' tt '+lit(33)+O(JE)+'pun2'+' tt '+lit(43)+O(JE)+'pun3'
  +O(JMP)+'pprim'+'\n')
w('pun1:'+' adv '+lit(LIT)+O(CALL)+'eg'+lit(0)+O(CALL)+'eint'+lit(32)+O(CALL)+'putchar'+O(DRP)
  +O(CALL)+'punary'+lit(SUB)+O(CALL)+'eg'+O(RET)+'\n')
w('pun2:'+' adv '+O(CALL)+'punary'+lit(CALL)+O(CALL)+'eg'+O(STR)+'"lnot"'+O(CALL)+'estr'
  +lit(32)+O(CALL)+'putchar'+O(DRP)+O(RET)+'\n')
w('pun3:'+' adv '+O(CALL)+'punary'+O(RET)+'\n')
# primary
w('pprim:'+' tt '+lit(2)+O(JE)+'ppn'+' tt '+lit(3)+O(JE)+'pps'
  +' tt '+lit(40)+O(JE)+'ppp'+' tt '+lit(1)+O(JE)+'ppi'
  +O(STR)+'"bad primary"'+O(CALL)+'die'+'\n')
w('ppn:'+lit(LIT)+O(CALL)+'eg'+' tv '+O(CALL)+'eint'+lit(32)+O(CALL)+'putchar'+O(DRP)+' adv '+O(RET)+'\n')
w('pps:'+lit(STR)+O(CALL)+'eg'+lit(34)+O(CALL)+'putchar'+O(DRP)+' tv '+O(CALL)+'ename'
  +lit(34)+O(CALL)+'putchar'+O(DRP)+lit(32)+O(CALL)+'putchar'+O(DRP)+' adv '+O(RET)+'\n')
w('ppp:'+' adv '+O(CALL)+'por'+lit(41)+O(CALL)+'exp'+O(RET)+'\n')
w('ppi:'+' tv '+O(CALL)+'lpush'+' adv '+' tt '+lit(40)+O(JE)+'pq'
  +O(CALL)+'lpop'+O(CALL)+'findvar'+O(CALL)+'egets'+O(RET)+'\n')
w('pq:'+' adv '+'\n')
w('ppal:'+' tt '+lit(41)+O(JE)+'ppad'+O(CALL)+'por'
  +' tt '+lit(44)+O(JE)+'ppac'+O(JMP)+'ppal'+'\n')
w('ppac:'+' adv '+O(JMP)+'ppal'+'\n')
w('ppad:'+lit(41)+O(CALL)+'exp'+lit(CALL)+O(CALL)+'eg'+O(CALL)+'lpop'+O(DUP)+O(CALL)+'isvoid'+sv(ISV)+O(CALL)+'ename'
  +lit(32)+O(CALL)+'putchar'+O(DRP)+O(RET)+'\n')

# ---------- main ----------
w('main:')
# argc >= 2
w(O(EXTERN)+'"nkr_argc"'+O(LOADX)+lit(2)+O(CALL)+'zlt'+O(JZ)+'mok'
  +O(STR)+'"usage: trans file.c"'+O(CALL)+'die'+'\n')
w('mok:'+O(EXTERN)+'"nkr_argv"'+O(LOADX)+lit(8)+O(ADD)+O(LOADX)
  +O(STR)+'"r"'+O(CALL)+'fopen'+sv(SCR)
  +gv(SCR)+lit(0)+lit(2)+O(CALL)+'fseek'+O(DRP)
  +gv(SCR)+O(CALL)+'ftell'+sv(N)
  +gv(SCR)+lit(0)+lit(0)+O(CALL)+'fseek'+O(DRP)
  +O(LIT)+'byte'+gv(N)+lit(1)+O(ADD)+O(ARR)+sv(SRC)
  +gv(SRC)+lit(16)+O(ADD)+sv(SRCDAT)
  +gv(SRCDAT)+lit(1)+gv(N)+gv(SCR)+O(CALL)+'fread'+O(DRP)
  +gv(SCR)+O(CALL)+'fclose'+O(DRP)
  +O(LIT)+'byte'+lit(256)+O(ARR)+sv(CT)
  +O(LIT)+'int'+lit(65536)+O(ARR)+sv(TT)
  +O(LIT)+'int'+lit(65536)+O(ARR)+sv(TVL)
  +O(LIT)+'byte'+lit(65536)+O(ARR)+sv(NTAB)
  +gv(NTAB)+lit(16)+O(ADD)+sv(NDAT)
  +O(LIT)+'int'+lit(8192)+O(ARR)+sv(NOFF)
  +O(LIT)+'int'+lit(8192)+O(ARR)+sv(NLENG)
  +O(LIT)+'int'+lit(64)+O(ARR)+sv(PVARS)
  +O(LIT)+'int'+lit(256)+O(ARR)+sv(VN)
  +O(LIT)+'int'+lit(256)+O(ARR)+sv(VS)
  +O(LIT)+'int'+lit(256)+O(ARR)+sv(LSTK)
  +O(EXTERN)+'"stdout"'+O(LOADX)+sv(STDOUT)
  +O(CALL)+'ctinit'
  +lit(0)+sv(POS)+lit(0)+sv(NTOK)+lit(0)+sv(PI)+lit(0)+sv(NUSED)
  +lit(0)+sv(NNAMES)+lit(0)+sv(NEXTVAR)+lit(0)+sv(LBL)+lit(0)+sv(VCNT)+lit(0)+sv(LCNT)+lit(0)+sv(ISV)
  +O(CALL)+'lx'+'\n')
# preamble of the emitted program
SHR63 = O(SHR)*63
preamble = (O(IMPORT)+'c"printf"(ptr,...)->int'+O(IMPORT)+'c"malloc"(int)->ptr'
  +O(IMPORT)+'c"free"(ptr)->void'+O(IMPORT)+'c"puts"(ptr)->int'+O(CALL)+'main'+O(RET)
  +'lt:'+O(SUB)+SHR63+O(RET)
  +'gt:'+O(SWP)+O(CALL)+'lt'+O(RET)
  +'le:'+O(SWP)+O(CALL)+'lt'+lit(1)+O(SWP)+O(SUB)+O(RET)
  +'ge:'+O(CALL)+'lt'+lit(1)+O(SWP)+O(SUB)+O(RET)
  +'eq:'+O(JE)+'eqy'+lit(0)+O(RET)+'eqy:'+lit(1)+O(RET)
  +'ne:'+O(JE)+'nen'+lit(1)+O(RET)+'nen:'+lit(0)+O(RET)
  +'lnot:'+lit(0)+O(JE)+'lny'+lit(0)+O(RET)+'lny:'+lit(1)+O(RET)
  +'nz:'+lit(0)+O(JE)+'nzz'+lit(1)+O(RET)+'nzz:'+lit(0)+O(RET)
  +'land:'+O(CALL)+'nz'+O(SWP)+O(CALL)+'nz'+O(MUL)+O(RET)
  +'lor:'+O(CALL)+'nz'+O(SWP)+O(CALL)+'nz'+O(ADD)+O(CALL)+'nz'+O(RET))
# emit preamble as escaped string literal
esc = preamble.replace('\\','\\\\').replace('"','\\"').replace('\n','\\n')
w(O(STR)+'"'+esc+'"'+O(CALL)+'estr'+lit(10)+O(CALL)+'putchar'+O(DRP)+'\n')
w(O(CALL)+'pprog'+lit(0)+O(RET)+'\n')

import re
txt = ''.join(out)
# macro invocations adjacent to ASCII idents/digits would fold into one ident; force a space
txt = re.sub(r'(?<![A-Za-z0-9_])(tt2|s63|adv|nx|pc|cl|tv|tt(?!2))(?=[A-Za-z0-9_])', r'\1 ', txt)
txt = re.sub(r'(?<=[A-Za-z0-9_])(tt2|s63|adv|nx|pc|cl|tv|tt)(?![A-Za-z0-9_])', r' \1', txt)
open('/home/chase/Projects/uflux/trans/trans.uf','w',encoding='utf-8').write(txt)
print("written", sum(len(x) for x in out), "chars")
