# AuRaw privacy notice / Datenschutzhinweise

Version: 22 July 2026

## English

### Controller and contact

AuRaw is published by Duecki. Privacy enquiries can be sent to
`dueckis@dueckis.de`.

Distributors who publish their own build may become responsible for their
distribution and must replace or supplement this contact information where
required.

### Local processing

AuRaw processes RAW photographs, edits, masks, previews, exports, and AI model
inference locally on the user's device. AuRaw does not contain analytics,
advertising, user accounts, or telemetry, and does not upload photographs,
masks, brush strokes, or prompts.

The app stores settings, downloaded models, thumbnails, and working caches on
the device. Image edit sidecars and exports are stored in the locations chosen
or described by the app. Users can remove this local data using their operating
system's file management and app-data controls. Uninstalling may not remove
exports or files deliberately stored in public folders.

### Optional model downloads

AI selection and inpainting are optional. If a required model is absent, AuRaw
shows a notice before making a network request. Nothing is downloaded when the
user cancels.

After the user chooses **Consent, download and continue**, the device connects
directly to one of these providers over HTTPS:

- GitHub, for the BiRefNet subject-selection model;
- Hugging Face, for the SAM 2.1 object-selection models and LaMa inpainting
  model.

The provider necessarily receives the device's public IP address and may record
the request time, network/device information, and other service-usage data. The
provider processes that data under its own privacy terms. AuRaw's request does
not include a photograph, mask, prompt, AuRaw account identifier, or telemetry.
The download is initiated on the basis of the user's optional, informed choice;
it can be refused by selecting **Cancel** without affecting non-AI editing.

- [GitHub General Privacy Statement](https://docs.github.com/en/site-policy/privacy-policies/github-general-privacy-statement)
- [Hugging Face Privacy Policy](https://huggingface.co/privacy)

Downloaded models remain cached locally until the user deletes the app data or
the model files. Model names, sources, licenses, sizes, and cryptographic
verification are documented in `THIRD_PARTY_NOTICES.md` and in the download
dialogue.

### Network access and recipients

AuRaw makes no other intentional runtime network requests. The model-hosting
providers are independent recipients and may process data outside Germany or
the European Economic Area according to their published privacy terms. Please
consult those terms for retention periods, legal bases, international-transfer
mechanisms, and how to exercise rights against the provider.

For questions about AuRaw's own processing or requests concerning locally held
app data, use the contact above. Users may also contact the data-protection
supervisory authority responsible for their place of residence.

## Deutsch

### Verantwortlicher und Kontakt

AuRaw wird von Duecki veröffentlicht. Datenschutzanfragen können an
`dueckis@dueckis.de` gesendet werden.

Wer eigene Builds veröffentlicht, kann für diese Verbreitung selbst
verantwortlich werden und muss diese Kontaktangaben gegebenenfalls ersetzen
oder ergänzen.

### Lokale Verarbeitung

AuRaw verarbeitet RAW-Fotos, Bearbeitungen, Masken, Vorschauen, Exporte und die
KI-Modellausführung lokal auf dem Gerät. AuRaw enthält keine Analyse-, Werbe-,
Konto- oder Telemetriefunktionen. Fotos, Masken, Pinselstriche und Prompts werden
nicht hochgeladen.

Einstellungen, heruntergeladene Modelle, Vorschaubilder und Arbeits-Caches
werden lokal gespeichert. Sidecar-Dateien und Exporte liegen an den in der App
gewählten oder beschriebenen Orten. Diese Daten können über die Datei- und
App-Datenverwaltung des Betriebssystems gelöscht werden. Eine Deinstallation
entfernt möglicherweise keine Exporte oder bewusst in öffentlichen Ordnern
gespeicherten Dateien.

### Optionale Modelldownloads

KI-Auswahl und Inpainting sind optional. Fehlt ein Modell, zeigt AuRaw vor jeder
Netzwerkverbindung einen Hinweis. Bei **Abbrechen** findet kein Download statt.

Nach **Consent, download and continue** („Zustimmen, herunterladen und
fortfahren“) verbindet sich das Gerät per HTTPS direkt mit:

- GitHub für das BiRefNet-Modell zur Motivauswahl;
- Hugging Face für die SAM-2.1-Modelle zur Objektauswahl und das LaMa-Modell
  für Inpainting.

Der Anbieter erhält technisch notwendig die öffentliche IP-Adresse und kann
Zeitpunkt, Netzwerk-/Geräteinformationen und weitere Nutzungsdaten erfassen.
Diese Verarbeitung erfolgt nach den Datenschutzbestimmungen des Anbieters.
AuRaw übermittelt dabei weder Foto, Maske oder Prompt noch eine AuRaw-Kennung
oder Telemetrie. Der optionale Download erfolgt erst nach der informierten
Entscheidung des Nutzers. Er kann ohne Einschränkung der übrigen
Bildbearbeitung abgelehnt werden.

- [Datenschutzerklärung von GitHub](https://docs.github.com/de/site-policy/privacy-policies/github-general-privacy-statement)
- [Datenschutzerklärung von Hugging Face](https://huggingface.co/privacy)

Die Modelle bleiben lokal im Cache, bis die App-Daten oder Modelldateien
gelöscht werden. Modellname, Quelle, Lizenz, Größe und kryptografische Prüfung
sind in `THIRD_PARTY_NOTICES.md` und im Downloadfenster angegeben.

### Netzwerkzugriffe und Empfänger

AuRaw führt zur Laufzeit keine weiteren beabsichtigten Netzwerkzugriffe aus.
Die Modell-Hosting-Anbieter sind eigenständige Empfänger und können Daten nach
Maßgabe ihrer veröffentlichten Bestimmungen außerhalb Deutschlands oder des
Europäischen Wirtschaftsraums verarbeiten. Angaben zu Speicherdauer,
Rechtsgrundlage, Drittlandtransfer und Betroffenenrechten stehen in den
Datenschutzerklärungen der Anbieter.

Fragen zur Verarbeitung durch AuRaw und Anfragen zu lokal gespeicherten
App-Daten können an den oben genannten Kontakt gerichtet werden. Betroffene
können sich außerdem an die für ihren Wohnort zuständige
Datenschutzaufsichtsbehörde wenden.
