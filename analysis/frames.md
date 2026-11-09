# DDE3 protocol frames (extracted via `tools/prg_vm.py`)

Generic OK response fed to each job: `92 04 A0` + zero padding.
Frames a job patches from the response show `00` data fields —
this captures the frame **structure** (address / service / length),
not the real payload values.

## EDIC_RESET  `@ 0x00a0`
  - (no `xsend` captured before stop)
  - vars: {"JOB_STATUS": "OKAY\u0000"}

## INITIALISIERUNG  `@ 0x00db`
  - (no `xsend` captured before stop)
  - vars: {"DONE": 1}

## INFO  `@ 0x0189`
  - (no `xsend` captured before stop)
  - vars: {"ECU": "DDE30\u0000", "ORIGIN": "BMW TP-421 Weber/Mellersh; Softing GmbH\u0000", "REVISION": "1.0\u0000", "AUTHOR": "BMW TP-421 Weber; Softing GmbH\u0000", "COMMENT": "Kennfeldprogrammierung DDE DS2/Flash DDE3.0\u0000", "SPRACHE": "deutsch\u0000", "JOB_STATUS": "OKAY\u0000"}

## UPROG_EIN  `@ 0x02e7`
  - `0x000308`  `12 04 0d`
  - `0x000680`  `12 09 06 00 00 00 00 02`
  - vars: {"JOB_STATUS": "OKAY", "STATUS_UPROG_WERT": 0.0, "STATUS_UPROG_EINH": "V\u0000"}

## UPROG_LESEN  `@ 0x0acf`
  - `0x000af0`  `12 04 0d`
  - `0x000e68`  `12 09 06 00 00 00 00 02`
  - vars: {"JOB_STATUS": "OKAY", "STATUS_UPROG_WERT": 0.0, "STATUS_UPROG_EINH": "V\u0000"}

## BAUDRATEN_UMSTELLUNG  `@ 0x121c`
  - ⚠️ stopped early: `KeyError: 'param 1 not provided'`
  - (no `xsend` captured before stop)

## BAUDRATEN_LESEN  `@ 0x1644`
  - ⚠️ stopped early: `KeyError: 'param 1 not provided'`
  - (no `xsend` captured before stop)

## UPROG_AUS  `@ 0x2209`
  - (no `xsend` captured before stop)
  - vars: {"JOB_STATUS": "OKAY\u0000"}

## BLOCKLAENGE_MAX  `@ 0x2259`
  - `0x00227a`  `12 04 0d`
  - `0x0025f2`  `12 09 06 00 00 00 00 01`
  - vars: {"JOB_STATUS": "OKAY", "BLOCKLAENGE_MAX_WERT": 65527}

## DATEN_REFERENZ  `@ 0x2931`
  - `0x002952`  `12 04 0d`
  - `0x002cca`  `12 09 06 00 00 00 00 07`
  - vars: {"JOB_STATUS": "OKAY"}

## HW_REFERENZ  `@ 0x337b`
  - `0x00339c`  `12 04 0d`
  - `0x003714`  `12 09 06 00 00 00 00 07`
  - vars: {"JOB_STATUS": "OKAY"}

## ZIF  `@ 0x3ba5`
  - `0x003bc6`  `12 04 0d`
  - `0x003f3e`  `12 09 06 00 00 00 00 46`
  - vars: {"JOB_STATUS": "OKAY"}

## ZIF_BACKUP  `@ 0x469f`
  - `0x0046c0`  `12 04 0d`
  - `0x004a38`  `12 09 06 00 00 00 00 46`
  - vars: {"JOB_STATUS": "OKAY"}

## AIF_LESEN  `@ 0x51ed`
  - ⚠️ stopped early: `KeyError: 'param 1 not provided'`
  - (no `xsend` captured before stop)

## AIF_SCHREIBEN  `@ 0x8445`
  - ⚠️ stopped early: `KeyError: 'param 1 not provided'`
  - (no `xsend` captured before stop)

## STATUS_CODIER_CHECKSUMME  `@ 0xc080`
  - `0x00c0a1`  `12 04 0a`
  - vars: {"JOB_STATUS": "OKAY\u0000", "STATUS_CHECKSUMME_WERT": 0}

## FLASH_LESEN  `@ 0xc33c`
  - ⚠️ stopped early: `KeyError: 'param 1 not provided'`
  - (no `xsend` captured before stop)

## FLASH_LOESCHEN  `@ 0xc79f`
  - `0x00caea`  `12 09 07 06 00 00 00 00`
  - vars: {"JOB_STATUS": "OKAY", "FLASH_LOESCHEN_ANZAHL": 0, "FLASH_LOESCHEN_ADRESSE": "\u0000\u0000\u0000", "FLASH_LOESCHEN_STATUS": 0}

## FLASH_SCHREIBEN  `@ 0xce81`
  - `0x00d379`  `12 09 07 02 00 00 00 00`
  - vars: {"JOB_STATUS": "OKAY", "FLASH_SCHREIBEN_ANZAHL": 0, "FLASH_SCHREIBEN_ADRESSE": "\u0000\u0000\u0000", "FLASH_SCHREIBEN_STATUS": 0}

## FLASH_SCHREIBEN_ENDE  `@ 0xd782`
  - `0x00da85`  `12 09 07 0f 00 00 00 00`
  - vars: {"JOB_STATUS": "OKAY", "FLASH_SCHREIBEN_ENDE_ANZAHL": 0, "FLASH_SCHREIBEN_ENDE_ADRESSE": "\u0000\u0000\u0000", "FLASH_SCHREIBEN_ENDE_STATUS": 0}

## STATUS_LESEN  `@ 0xdde6`
  - `0x00de29`  `12 04 0d`
  - `0x00e2d4`  `12 09 07 0f 00 00 00 00`
  - vars: {"JOB_STATUS": "OKAY", "SG_STATUS": 0}

## DATENBEREICH_LOESCHEN_0E  `@ 0xe538`
  - `0x00e55e`  `12 09 07 0e 42 41 50 00`
  - vars: {"JOB_STATUS": "OKAY"}

## SEED_KEY  `@ 0xe786`
  - (no `xsend` captured before stop)

## IDENT  `@ 0xf3b4`
  - `0x00f3d5`  `12 04 00`
  - vars: {"JOB_STATUS": "OKAY"}

## ADAPTION_LOESCHEN  `@ 0x10050`
  - (no `xsend` captured before stop)
  - vars: {"JOB_STATUS": "OKAY\u0000"}

## FS_LESEN  `@ 0x1007a`
  - `0x01009c`  `12 05 04 01`
  - vars: {"JOB_STATUS": "OKAY\u0000"}

## FS_LOESCHEN  `@ 0x10559`
  - `0x01057a`  `12 04 05`
  - vars: {"JOB_STATUS": "OKAY"}

## DIAGNOSE_ENDE  `@ 0x106bb`
  - `0x0106dc`  `12 04 9f`
  - vars: {"JOB_STATUS": "OKAY"}

## SCHALTE_PROTOKOLL_DS2_NACH_K2  `@ 0x1081d`
  - `0x01083f`  `12 05 90 56`
  - vars: {"JOB_STATUS": "OKAY\u0000"}

## SCHALTE_K2_DS2  `@ 0x10887`
  - `0x010987`  `08 00 02 53 01 0c 20 02`
  - vars: {"JOB_STATUS": "ERROR_NACK\u0000"}

## SET_EDIC_BAUDRATE  `@ 0x10b63`
  - ⚠️ stopped early: `KeyError: 'param 1 not provided'`
  - (no `xsend` captured before stop)

## STEUERN_SYNC_MODE  `@ 0x10de3`
  - ⚠️ stopped early: `KeyError: 'param 1 not provided'`
  - (no `xsend` captured before stop)

## STATUS_SYNC_MODE  `@ 0x115bf`
  - `0x0115ee`  `12 04 6d`
  - vars: {"JOB_STATUS": "OKAY", "STATUS_SYNC_MODE_STATUS": 1, "STATUS_SYNC_MODE_TEXT": "Startwert verstanden, EWS und DME passen nicht zusammen\u0000"}
