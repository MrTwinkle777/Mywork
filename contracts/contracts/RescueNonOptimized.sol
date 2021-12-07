//SPDX-License-Identifier: MIT OR Apache-2.0
pragma solidity ^0.8.0;

import "hardhat/console.sol";

contract RescueNonOptimized {
    /// The constants are obtained from the Sage script
    /// https://gitlab.com/translucence/crypto/marvellous/-/blob/b0885058f0348171befcf6cf30533812c9f49e15/rescue254.sage

    uint256 internal constant N_ROUNDS = 12;
    uint256 internal constant STATE_SIZE = 4;
    uint256 internal constant SCHEDULED_KEY_SIZE =
        (2 * N_ROUNDS + 1) * STATE_SIZE;
    uint256 internal constant MDS_SIZE = STATE_SIZE * STATE_SIZE;

    // Obtained by running KeyScheduling([0,0,0,0]). See Algorithm 2 of AT specification document.
    // solhint-disable-next-line var-name-mixedcase
    uint256[SCHEDULED_KEY_SIZE] internal SCHEDULED_KEY = [
        // Init key injection
        14613516837064033601098425266946467918409544647446217386229959902054563533267,
        376600575581954944138907282479272751264978206975465380433764825531344567663,
        7549886658634274343394883631367643327196152481472281919735617268044202589860,
        3682071510138521345600424597536598375718773365536872232193107639375194756918,
        // i = 0
        18657517374128716281071590782771170166993445602755371021955596036781411817786,
        7833794394096838639430144230563403530989402760602204539559270044687522640191,
        21303828694647266539931030987057572024333442749881970102454081226349775826204,
        10601447988834057856019990466870413629636256450824419416829818546423193802418,
        // i = 1
        3394657260998945409283098835682964352503279447198495330506177586645995289229,
        18437084083724939316390841967750487133622937044030373241106776324730657101302,
        9281739916935170266925270432337475828741505406943764438550188362765269530037,
        7363758719535652813463843693256839865026387361836644774317493432208443086206,
        // i = 2
        307094088106440279963968943984309088038734274328527845883669678290790702381,
        20802277384865839022876847241719852837518994021170013346790603773477912819001,
        19754579269464973651593381036132218829220609572271224048608091445854164824042,
        3618840933841571232310395486452077846249117988789467996234635426899783130819,
        // i = 3
        2604166168648013711791424714498680546427073388134923208733633668316805639713,
        21355705619901626246699129842094174300693414345856149669339147704587730744579,
        492957643799044929042114590851019953669919577182050726596188173945730031352,
        8495959434717951575638107349559891417392372124707619959558593515759091841138,
        // i = 4
        15608173629791582453867933160400609222904457931922627396107815347244961625587,
        16346164988481725869223011419855264063160651334419415042919928342589111681923,
        21085652277104054699752179865196164165969290053517659864117475352262716334100,
        20640310021063232205677193759981403045043444605175178332133134865746039279935,
        // i = 5
        6015589261538006311719125697023069952804098656652050863009463360598997670240,
        12498423882721726012743791752811798719201859023192663855805526312393108407357,
        10785527781711732350693172404486938622378708235957779975342240483505724965040,
        5563181134859229953817163002660048854420912281911747312557025480927280392569,
        // i = 6
        4585980485870975597083581718044393941512074846925247225127276913719050121968,
        8135760428078872176830812746579993820254685977237403304445687861806698035222,
        4525715538433244696411192727226186804883202134636681498489663161593606654720,
        2537497100749435007113677475828631400227339157221711397900070636998427379023,
        // i = 7
        6957758175844522415482704083077249782181516476067074624906502033584870962925,
        17134288156316028142861248367413235848595762718317063354217292516610545487813,
        20912428573104312239411321877435657586184425249645076131891636094671938892815,
        16000236205755938926858829908701623009580043315308207671921283074116709575629,
        // i = 8
        10226182617544046880850643054874064693998595520540061157646952229134207239372,
        18584346134948015676264599354709457865255277240606855245909703396343731224626,
        9263628039314899758000383385773954136696958567872461042004915206775147151562,
        21095966719856094705113273596585696209808876361583941931684481364905087347856,
        // i = 9
        2671157351815122058649197205531097090514563992249109660044882868649840700911,
        19371695134219415702961622134896564229962454573253508904477489696588594622079,
        5458968308231210904289987830881528056037123818964633914555287871152343390175,
        7336332584551233792026746889434554547883125466404119632794862500961953384162,
        // i = 10
        10351436748086126474964482623536554036637945319698748519226181145454116702488,
        10588209357420186457766745724579739104572139534486480334142455690083813419064,
        14330277147584936710957102218096795520430543834717433464500965846826655802131,
        20752197679372238381408962682213349118865256502118746003818603260257076802028,
        // i = 11
        19390446529582160674621825412345750405397926216690583196542690617266028463414,
        4169994013656329171830126793466321040216273832271989491631696813297571003664,
        3014817248268674641565961681956715664833306954478820029563459099892548946802,
        14285412497877984113655094566695921704826935980354186365694472961163628072901,
        // i = 12
        16224484149774307577146165975762490690838415946665379067259822320752729067513,
        5404416528124718330316441408560295270695591369912905197499507811036327404407,
        20127204244332635127213425090893250761286848618448128307344971109698523903374,
        14939477686176063572999014162186372798386193194442661892600584389296609365740,
        // i = 13
        183740587182448242823071506013879595265109215202349952517434740768878294134,
        15366166801397358994305040367078329374182896694582870542425225835844885654667,
        10066796014802701613007252979619633540090232697942390802486559078446300507813,
        4824035239925904398047276123907644574421550988870123756876333092498925242854,
        // i = 14
        5526416022516734657935645023952329824887761902324086126076396040056459740202,
        18157816292703983306114736850721419851645159304249709756659476015594698876611,
        767446206481623130855439732549764381286210118638028499466788453347759203223,
        16303412231051555792435190427637047658258796056382698277687500021321460387129,
        // i = 15
        15475465085113677237835653765189267963435264152924949727326000496982746660612,
        14574823710073720047190393602502575509282844662732045439760066078137662816054,
        13746490178929963947720756220409862158443939172096620003896874772477437733602,
        13804898145881881347835367366352189037341704254740510664318597456840481739975,
        // i = 16
        3523599105403569319090449327691358425990456728660349400211678603795116364226,
        8632053982708637954870974502506145434219829622278773822242070316888003350278,
        20293222318844554840191640739970825558851264905959070636369796127300969629060,
        7583204376683983181255811699503668584283525661852773339144064901897953897564,
        // i = 17
        7562572155566079175343789986900217168516831778275127159068657756836798778249,
        12689811910161401007144285031988539999455902164332232460061366402869461973371,
        21878400680687418538050108788381481970431106443696421074205107984690362920637,
        3428721187625124675258692786364137915132424621324969246210899039774126165479,
        // i = 18
        2552744099402346352193097862110515290335034445517764751557635302899937367219,
        13706727374402840004346872704605212996406886221231239230397976011930486183550,
        19786308443934570499119114884492461847023732197118902978413499381102456961966,
        11767081169862697956461405434786280425108140215784390008330611807075539962898,
        // i = 19
        1273319740931699377003430019539548781935202579355152343831464213279794249000,
        20225620070386241931202098463018472034137960205721651875253423327929063224115,
        13107884970924459680133954992354588464904218518440707039430314610799573960437,
        10574066469653966216567896842413898230152427846140046825523989742590727910280,
        // i = 20
        21386271527766270535632132320974945129946865648321206442664310421414128279311,
        15743262855527118149527268525857865250723531109306484598629175225221686341453,
        16251140915157602891864152518526119259367827194524273940185283798897653655734,
        5420158299017134702074915284768041702367316125403978919545323705661634647751,
        // i = 21
        14555572526833606349832007897859411042036463045080050783981107823326880950231,
        15234942318869557310939446038663331226792664588406507247341043508129993934298,
        19560004467494472556570844694553210033340577742756929194362924850760034377042,
        21851693551359717578445799046408060941161959589978077352548456186528047792150,
        // i = 22
        19076469206110044175016166349949136119962165667268661130584159239385341119621,
        19132104531774396501521959463346904008488403861940301898725725957519076019017,
        6606159937109409334959297158878571243749055026127553188405933692223704734040,
        13442678592538344046772867528443594004918096722084104155946229264098946917042,
        // i = 23
        11975757366382164299373991853632416786161357061467425182041988114491638264212,
        10571372363668414752587603575617060708758897046929321941050113299303675014148,
        5405426474713644587066466463343175633538103521677501186003868914920014287031,
        18665277628144856329335676361545218245401014824195451740181902217370165017984
    ];

    // solhint-disable-next-line var-name-mixedcase
    uint256[MDS_SIZE] internal MDS = [
        21888242871839275222246405745257275088548364400416034343698204186575808479992,
        21888242871839275222246405745257275088548364400416034343698204186575806058117,
        21888242871839275222246405745257275088548364400416034343698204186575491214367,
        21888242871839275222246405745257275088548364400416034343698204186535831058117,
        19500,
        3026375,
        393529500,
        49574560750,
        21888242871839275222246405745257275088548364400416034343698204186575808491587,
        21888242871839275222246405745257275088548364400416034343698204186575807886437,
        21888242871839275222246405745257275088548364400416034343698204186575729688812,
        21888242871839275222246405745257275088548364400416034343698204186565891044437,
        156,
        20306,
        2558556,
        320327931
    ];

    uint256 internal constant PRIME =
        21888242871839275222246405745257275088548364400416034343698204186575808495617;

    uint256 internal constant ALPHA = 5;

    uint256 internal constant ALPHA_INV =
        17510594297471420177797124596205820070838691520332827474958563349260646796493;

    function expMod(
        uint256 base,
        uint256 e,
        uint256 m
    ) public view returns (uint256 o) {
        assembly {
            // define pointer
            let p := mload(0x40)
            // store data assembly-favouring ways
            mstore(p, 0x20) // Length of Base
            mstore(add(p, 0x20), 0x20) // Length of Exponent
            mstore(add(p, 0x40), 0x20) // Length of Modulus
            mstore(add(p, 0x60), base) // Base
            mstore(add(p, 0x80), e) // Exponent
            mstore(add(p, 0xa0), m) // Modulus
            if iszero(staticcall(sub(gas(), 2000), 0x05, p, 0xc0, p, 0x20)) {
                revert(0, 0)
            }
            // data
            o := mload(p)
        }
    }

    function addVectors(
        uint256[STATE_SIZE] memory v1,
        uint256[STATE_SIZE] memory v2
    ) internal pure returns (uint256[STATE_SIZE] memory) {
        uint256[STATE_SIZE] memory v;

        for (uint256 j = 0; j < STATE_SIZE; j++) {
            v[j] = addmod(v1[j], v2[j], PRIME);
        }

        return v;
    }

    // MDS is hardcoded
    function linearOp(
        uint256[STATE_SIZE] memory state,
        uint256[STATE_SIZE] memory key
    ) private view returns (uint256[STATE_SIZE] memory) {
        uint256[STATE_SIZE] memory newState = [uint256(0), 0, 0, 0];

        // TODO remove loop to save gas?
        // Matrix multiplication
        for (uint256 i = 0; i < STATE_SIZE; i++) {
            uint256 sum = uint256(0);
            for (uint256 j = 0; j < STATE_SIZE; j++) {
                sum = addmod(
                    sum,
                    mulmod(MDS[i * STATE_SIZE + j], state[j], PRIME),
                    PRIME
                );
            }
            newState[i] = sum;
        }

        // Add constant
        newState = addVectors(newState, key);

        return newState;
    }

    // Computes the Rescue permutation on some input
    // Recall that the scheduled key is precomputed in our case
    // @param input input for the permutation
    // @return permutation output
    function perm(
        uint256[STATE_SIZE] memory input // TODO this should be made private/internal
    ) public view returns (uint256[STATE_SIZE] memory) {
        uint256[STATE_SIZE] memory key0 = [
            SCHEDULED_KEY[0],
            SCHEDULED_KEY[1],
            SCHEDULED_KEY[2],
            SCHEDULED_KEY[3]
        ];

        // S = m + k[0]
        uint256[STATE_SIZE] memory S = addVectors(input, key0); // solhint-disable-line var-name-mixedcase

        // Main loop
        for (uint256 i = 1; i < 2 * N_ROUNDS + 1; i++) {
            if ((i - 1) % 2 == 0) {
                // TODO avoid code duplication?
                S[0] = expMod(S[0], ALPHA_INV, PRIME);
                S[1] = expMod(S[1], ALPHA_INV, PRIME);
                S[2] = expMod(S[2], ALPHA_INV, PRIME);
                S[3] = expMod(S[3], ALPHA_INV, PRIME);
            } else {
                // TODO avoid code duplication?
                S[0] = expMod(S[0], ALPHA, PRIME);
                S[1] = expMod(S[1], ALPHA, PRIME);
                S[2] = expMod(S[2], ALPHA, PRIME);
                S[3] = expMod(S[3], ALPHA, PRIME);
            }

            uint256[STATE_SIZE] memory keyI = [
                SCHEDULED_KEY[i * STATE_SIZE],
                SCHEDULED_KEY[i * STATE_SIZE + 1],
                SCHEDULED_KEY[i * STATE_SIZE + 2],
                SCHEDULED_KEY[i * STATE_SIZE + 3]
            ];
            S = linearOp(S, keyI);
        }
        return S;
    }

    // Computes the hash of three field elements and returns a single element
    // In our case the rate is 3 and the capacity is 1
    // This hash function the one used in the Records Merkle tree.
    // @param a first element
    // @param b second element
    // @param c third element
    // @return the first element of the Rescue state
    function hash(
        uint256 a,
        uint256 b,
        uint256 c
    ) public returns (uint256) {
        uint256[STATE_SIZE] memory input;
        uint256[STATE_SIZE] memory state;

        input[0] = a;
        input[1] = b;
        input[2] = c;
        input[3] = 0;

        state = perm(input);

        return state[0];
    }
}
